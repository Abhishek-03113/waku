use std::ops::Range;
use std::time::Duration;

use crate::md::highlight::{self, Lang, TokenClass};
use crate::ui::menu::{ContextMenuHandle, MenuItem, context_menu};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, SharedString, StyledText, Subscription,
    Task, TextLayout, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point,
    prelude::*, px, size,
};
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
        MoveToPreviousWord,
        MoveToNextWord,
        SelectToStart,
        SelectToEnd,
        SelectToPreviousWord,
        SelectToNextWord,
        DeleteToStart,
        DeleteToEnd,
        DeleteToPreviousWord,
        DeleteToNextWord,
        Paste,
        Cut,
        Copy,
        Enter,
    ]
);

const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const CURSOR_BLINK_PAUSE: Duration = Duration::from_millis(300);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("ComposerInput")),
        KeyBinding::new("delete", Delete, Some("ComposerInput")),
        KeyBinding::new("cmd-backspace", DeleteToStart, Some("ComposerInput")),
        KeyBinding::new("cmd-delete", DeleteToEnd, Some("ComposerInput")),
        KeyBinding::new("alt-backspace", DeleteToPreviousWord, Some("ComposerInput")),
        KeyBinding::new("alt-delete", DeleteToNextWord, Some("ComposerInput")),
        KeyBinding::new("ctrl-h", Backspace, Some("ComposerInput")),
        KeyBinding::new("ctrl-d", Delete, Some("ComposerInput")),
        KeyBinding::new("ctrl-u", DeleteToStart, Some("ComposerInput")),
        KeyBinding::new("ctrl-k", DeleteToEnd, Some("ComposerInput")),
        KeyBinding::new("left", Left, Some("ComposerInput")),
        KeyBinding::new("right", Right, Some("ComposerInput")),
        KeyBinding::new("ctrl-b", Left, Some("ComposerInput")),
        KeyBinding::new("ctrl-f", Right, Some("ComposerInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("ComposerInput")),
        KeyBinding::new("shift-right", SelectRight, Some("ComposerInput")),
        KeyBinding::new("home", Home, Some("ComposerInput")),
        KeyBinding::new("end", End, Some("ComposerInput")),
        KeyBinding::new("cmd-left", Home, Some("ComposerInput")),
        KeyBinding::new("cmd-right", End, Some("ComposerInput")),
        KeyBinding::new("cmd-up", Home, Some("ComposerInput")),
        KeyBinding::new("cmd-down", End, Some("ComposerInput")),
        KeyBinding::new("ctrl-a", Home, Some("ComposerInput")),
        KeyBinding::new("ctrl-e", End, Some("ComposerInput")),
        KeyBinding::new("shift-home", SelectToStart, Some("ComposerInput")),
        KeyBinding::new("shift-end", SelectToEnd, Some("ComposerInput")),
        KeyBinding::new("shift-cmd-left", SelectToStart, Some("ComposerInput")),
        KeyBinding::new("shift-cmd-right", SelectToEnd, Some("ComposerInput")),
        KeyBinding::new("cmd-shift-up", SelectToStart, Some("ComposerInput")),
        KeyBinding::new("cmd-shift-down", SelectToEnd, Some("ComposerInput")),
        KeyBinding::new("ctrl-shift-a", SelectToStart, Some("ComposerInput")),
        KeyBinding::new("ctrl-shift-e", SelectToEnd, Some("ComposerInput")),
        KeyBinding::new("alt-left", MoveToPreviousWord, Some("ComposerInput")),
        KeyBinding::new("alt-right", MoveToNextWord, Some("ComposerInput")),
        KeyBinding::new(
            "alt-shift-left",
            SelectToPreviousWord,
            Some("ComposerInput"),
        ),
        KeyBinding::new("alt-shift-right", SelectToNextWord, Some("ComposerInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("ComposerInput")),
        KeyBinding::new("cmd-v", Paste, Some("ComposerInput")),
        KeyBinding::new("cmd-c", Copy, Some("ComposerInput")),
        KeyBinding::new("cmd-x", Cut, Some("ComposerInput")),
        KeyBinding::new("enter", Enter, Some("ComposerInput")),
    ]);
}

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
            cx.background_executor().timer(CURSOR_BLINK_PAUSE).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    this.paused = false;
                    this.blink(epoch, cx);
                });
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
            cx.background_executor().timer(CURSOR_BLINK_INTERVAL).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.blink(epoch, cx));
            }
        });
    }
}

#[derive(Clone)]
pub enum ComposerEvent {
    Submit(String),
    /// The field took focus. A code editor uses this to re-read its file, so
    /// clicking back into it picks up changes made on disk meanwhile.
    Focus,
    /// The text content actually changed. Parents that derive UI from the
    /// content — filter lists, draft state, dirty markers — react to this,
    /// never to raw notifies: the field also notifies for caret blinks and
    /// selection changes, and re-rendering an owner twice a second for a
    /// blinking caret is exactly the per-frame waste the field exists to
    /// contain.
    Edited,
}

/// What the field is for. The difference is small but load-bearing: Enter
/// submits a prompt, inserts a newline in code, and in a search field submits
/// while keeping the query.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FieldMode {
    #[default]
    Composer,
    Code,
    Search,
}

pub struct ComposerInput {
    focus_handle: FocusHandle,
    mode: FieldMode,
    read_only: bool,
    /// Language for paint-only syntax colouring, in code mode.
    language: Option<Lang>,
    /// Cached token spans over `content`, as absolute byte ranges. Recomputed
    /// only when the content changes, so painting a large file is free.
    highlight: Vec<(Range<usize>, TokenClass)>,
    /// Find-in-file match ranges painted as washes under the text, sorted and
    /// non-overlapping. Owned by the find bar, which recomputes them whenever
    /// the content or the query changes; the field only paints them.
    search_matches: Vec<Range<usize>>,
    /// Index into `search_matches` of the match navigation is on, painted
    /// stronger than its siblings.
    active_search_match: Option<usize>,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<TextLayout>,
    is_selecting: bool,
    selected_word_range: Option<Range<usize>>,
    external_context_menu_focus_holds: usize,
    context_menu: ContextMenuHandle,
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
            mode: FieldMode::Composer,
            read_only: false,
            language: None,
            highlight: Vec::new(),
            search_matches: Vec::new(),
            active_search_match: None,
            content: "".into(),
            placeholder: "Do anything…".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            is_selecting: false,
            selected_word_range: None,
            external_context_menu_focus_holds: 0,
            context_menu: {
                // The menu takes real focus while open, so the composer holds
                // its caret visible for the duration — otherwise right-clicking
                // the input looks like it defocused.
                let composer = cx.entity().downgrade();
                ContextMenuHandle::new(cx).on_toggle(move |open, window, cx| {
                    let _ = composer.update(cx, |composer: &mut Self, cx| {
                        if open {
                            composer.preserve_visual_focus_for_context_menu(window, cx);
                        } else {
                            composer.release_visual_focus_for_context_menu(window, cx);
                        }
                    });
                })
            },
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

    /// Placeholder shown while the field is empty.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Turn the field into a code editor: Enter inserts a newline instead of
    /// submitting, and `language` (when recognised) colours the text.
    pub fn code_editor(mut self, language: Option<&str>) -> Self {
        self.mode = FieldMode::Code;
        self.language = language.and_then(highlight::lang_for_tag);
        self
    }

    /// Turn the field into a search box: Enter submits without clearing, so
    /// "find next" keeps the query, and pasted line breaks become spaces.
    pub fn search_field(mut self) -> Self {
        self.mode = FieldMode::Search;
        self
    }

    /// Reject edits while still allowing selection and copy.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Flip read-only after construction. A file editor starts locked because
    /// its contents are still being read off the UI thread, and unlocks once
    /// the read lands and says the file is writable.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Re-tokenize after a content change. Cheap for a composer (no language),
    /// one linear pass for a code editor.
    fn refresh_highlight(&mut self) {
        let Some(language) = self.language else {
            return;
        };
        self.highlight.clear();
        let mut line_start = 0;
        for (line, tokens) in self
            .content
            .split('\n')
            .zip(highlight::tokenize(language, &self.content))
        {
            self.highlight.extend(tokens.into_iter().map(|token| {
                (
                    line_start + token.range.start..line_start + token.range.end,
                    token.class,
                )
            }));
            line_start += line.len() + 1;
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    /// The caret's byte offset into `content`.
    pub fn cursor(&self) -> usize {
        self.cursor_offset()
    }

    /// Splice `text` over `range` and put the caret after it. This is the
    /// autocompletion insert: unlike [`EntityInputHandler::replace_text_in_range`]
    /// it takes byte offsets and no window, so an action handler can call it.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let range = range.start.min(self.content.len())..range.end.min(self.content.len());
        if !self.content.is_char_boundary(range.start)
            || !self.content.is_char_boundary(range.end)
            || range.start > range.end
        {
            return;
        }
        self.content =
            (self.content[..range.start].to_owned() + text + &self.content[range.end..]).into();
        let offset = range.start + text.len();
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.marked_range = None;
        self.refresh_highlight();
        self.pause_blink_cursor(cx);
        cx.emit(ComposerEvent::Edited);
        cx.notify();
    }

    /// Replace the painted find-match washes. Ranges must be sorted and
    /// non-overlapping; `active` indexes into `matches`. Purely visual — the
    /// content is untouched, so no [`ComposerEvent::Edited`] is emitted.
    pub fn set_search_matches(
        &mut self,
        matches: Vec<Range<usize>>,
        active: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if self.search_matches == matches && self.active_search_match == active {
            return;
        }
        self.search_matches = matches;
        self.active_search_match = active;
        cx.notify();
    }

    pub fn selected_range(&self) -> Range<usize> {
        self.selected_range.clone()
    }

    /// Move the selection to `range`, as find-next does when it lands on a
    /// match. Ignored unless the range sits on character boundaries, so a
    /// selection computed against stale content cannot split a code point.
    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if range.start > range.end
            || range.end > self.content.len()
            || !self.content.is_char_boundary(range.start)
            || !self.content.is_char_boundary(range.end)
        {
            return;
        }
        self.selected_range = range;
        self.selection_reversed = false;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    /// Select the whole content, the way reopening a find bar re-arms its
    /// query for retyping. Unlike the `SelectAll` action this needs no window.
    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    /// Where `offset` sits in the window as of the last paint, with the line
    /// height, so a find bar can scroll a match into view. `None` until the
    /// field has painted once.
    pub fn position_for_offset(&self, offset: usize) -> Option<(Point<Pixels>, Pixels)> {
        let layout = self.last_layout.as_ref()?;
        let position = layout.position_for_index(offset.min(self.content.len()))?;
        Some((position, layout.line_height()))
    }

    /// Height of each logical line as laid out, so a gutter can put one number
    /// per line even when soft wrap gives a line several visual rows.
    ///
    /// Read from the previous frame's layout — a gutter is therefore one frame
    /// behind a reflow, which is invisible next to the reflow itself.
    pub fn wrapped_line_heights(&self) -> Vec<Pixels> {
        let Some(layout) = self.last_layout.as_ref() else {
            return Vec::new();
        };
        let line_height = layout.line_height();
        layout
            .line_layouts()
            .iter()
            .map(|line| line_height * (line.wrap_boundaries().len() + 1) as f32)
            .collect()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        let changed = !self.content.is_empty();
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.highlight.clear();
        self.pause_blink_cursor(cx);
        if changed {
            cx.emit(ComposerEvent::Edited);
        }
        cx.notify();
    }

    pub fn set_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        let content = content.into();
        let changed = self.content != content;
        self.content = content;
        let offset = self.content.len();
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.marked_range = None;
        self.refresh_highlight();
        self.pause_blink_cursor(cx);
        if changed {
            cx.emit(ComposerEvent::Edited);
        }
        cx.notify();
    }

    fn on_focus(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.blink_cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.emit(ComposerEvent::Focus);
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

    fn move_to_previous_word(
        &mut self,
        _: &MoveToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = if self.selected_range.is_empty() {
            previous_word_boundary(&self.content, self.cursor_offset())
        } else {
            self.selected_range.start
        };
        self.move_to(offset, cx);
    }

    fn move_to_next_word(&mut self, _: &MoveToNextWord, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected_range.is_empty() {
            next_word_boundary(&self.content, self.cursor_offset())
        } else {
            self.selected_range.end
        };
        self.move_to(offset, cx);
    }

    fn select_to_start(&mut self, _: &SelectToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn select_to_previous_word(
        &mut self,
        _: &SelectToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(
            previous_word_boundary(&self.content, self.cursor_offset()),
            cx,
        );
    }

    fn select_to_next_word(
        &mut self,
        _: &SelectToNextWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(next_word_boundary(&self.content, self.cursor_offset()), cx);
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

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(0, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_end(&mut self, _: &DeleteToEnd, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.content.len(), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_previous_word(
        &mut self,
        _: &DeleteToPreviousWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(
                previous_word_boundary(&self.content, self.cursor_offset()),
                cx,
            );
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_next_word(
        &mut self,
        _: &DeleteToNextWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(next_word_boundary(&self.content, self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            FieldMode::Code => {
                self.replace_text_in_range(None, "\n", window, cx);
            }
            // A search query survives its own submission — Enter means "find
            // next", not "send" — and stays untrimmed because leading or
            // trailing spaces are part of what is searched for.
            FieldMode::Search => {
                cx.emit(ComposerEvent::Submit(self.content.to_string()));
            }
            FieldMode::Composer => {
                let value = self.content.trim().to_owned();
                if !value.is_empty() {
                    cx.emit(ComposerEvent::Submit(value));
                    self.clear(cx);
                }
            }
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let text = match self.mode {
            // A composer is one prompt — and a search box one query — so
            // pasted line breaks become spaces.
            FieldMode::Composer | FieldMode::Search => text.replace(['\n', '\r'], " "),
            FieldMode::Code => text.replace('\r', ""),
        };
        self.replace_text_in_range(None, &text, window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            // Nothing here to copy. The composer holds focus almost all the
            // time, so propagating lets an outer handler — the transcript's
            // text selection — answer the keystroke instead of swallowing it.
            cx.propagate();
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_string(),
        ));
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
        self.selected_word_range = None;
        let offset = self.index_for_mouse_position(event.position);

        if event.click_count >= 3 {
            self.selected_range = 0..self.content.len();
            self.selection_reversed = false;
            self.selected_word_range = Some(self.selected_range.clone());
            self.pause_blink_cursor(cx);
            cx.notify();
            return;
        }

        if event.click_count == 2 {
            let range = word_range_at(&self.content, offset);
            self.selected_range = range.clone();
            self.selection_reversed = false;
            self.selected_word_range = (!range.is_empty()).then_some(range);
            self.pause_blink_cursor(cx);
            cx.notify();
            return;
        }

        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn on_context_mouse_down(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.selected_word_range = None;
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
        if let Some(word_range) = self.selected_word_range.as_ref() {
            self.selected_range.start = self.selected_range.start.min(word_range.start);
            self.selected_range.end = self.selected_range.end.max(word_range.end);
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

fn previous_word_boundary(content: &str, offset: usize) -> usize {
    content[..offset]
        .split_word_bound_indices()
        .rev()
        .find(|(_, segment)| !segment.chars().all(char::is_whitespace))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_word_boundary(content: &str, offset: usize) -> usize {
    content[offset..]
        .split_word_bound_indices()
        .find(|(_, segment)| !segment.chars().all(char::is_whitespace))
        .map(|(index, segment)| offset + index + segment.len())
        .unwrap_or(content.len())
}

fn word_range_at(content: &str, offset: usize) -> Range<usize> {
    content
        .split_word_bound_indices()
        .find_map(|(index, segment)| {
            let range = index..index + segment.len();
            range.contains(&offset).then_some(range)
        })
        .unwrap_or(offset..offset)
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
        if self.read_only {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let previous = self.content.clone();
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        let offset = range.start + new_text.len();
        self.selected_range = offset..offset;
        self.marked_range = None;
        self.refresh_highlight();
        self.pause_blink_cursor(cx);
        if previous != self.content {
            cx.emit(ComposerEvent::Edited);
        }
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
        let previous = self.content.clone();
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
        if previous != self.content {
            cx.emit(ComposerEvent::Edited);
        }
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

/// Find-match washes layered into [`input_text_runs`]: every match gets
/// `match_color`, and the one navigation is on gets `active_color`, which also
/// wins over the selection wash so the current match keeps its identity while
/// selected — the way a find widget conventionally paints it.
struct SearchPaint<'a> {
    matches: &'a [Range<usize>],
    active: Option<&'a Range<usize>>,
    match_color: Hsla,
    active_color: Hsla,
}

impl SearchPaint<'static> {
    fn none() -> Self {
        Self {
            matches: &[],
            active: None,
            match_color: gpui::transparent_black(),
            active_color: gpui::transparent_black(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn input_text_runs(
    display_len: usize,
    base_run: TextRun,
    selected_range: Option<&Range<usize>>,
    marked_range: Option<&Range<usize>>,
    selection_color: Hsla,
    highlight: &[(Range<usize>, TokenClass)],
    token_color: impl Fn(TokenClass) -> Hsla,
    search: SearchPaint,
) -> Vec<TextRun> {
    let mut boundaries = vec![0, display_len];
    for range in [selected_range, marked_range].into_iter().flatten() {
        boundaries.push(range.start.min(display_len));
        boundaries.push(range.end.min(display_len));
    }
    for (range, _) in highlight {
        boundaries.push(range.start.min(display_len));
        boundaries.push(range.end.min(display_len));
    }
    for range in search.matches {
        boundaries.push(range.start.min(display_len));
        boundaries.push(range.end.min(display_len));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    // Token and match lists are sorted and non-overlapping, and every one of
    // their edges is a boundary, so each window has exactly one candidate —
    // found by binary search, keeping this linear-ish in the token count
    // rather than quadratic.
    let covering_match = |start: usize, end: usize| -> bool {
        let index = search.matches.partition_point(|range| range.end <= start);
        search
            .matches
            .get(index)
            .is_some_and(|range| range.start <= start && range.end >= end)
    };

    boundaries
        .windows(2)
        .filter_map(|boundary| {
            let start = boundary[0];
            let end = boundary[1];
            let token_index = highlight.partition_point(|(range, _)| range.end <= start);
            let color = highlight
                .get(token_index)
                .filter(|(range, _)| range.start <= start && range.end >= end)
                .map_or(base_run.color, |(_, class)| token_color(*class));
            let background_color = if search
                .active
                .is_some_and(|range| range.start <= start && range.end >= end)
            {
                Some(search.active_color)
            } else if selected_range.is_some_and(|range| range.start < end && range.end > start) {
                Some(selection_color)
            } else if covering_match(start, end) {
                Some(search.match_color)
            } else {
                None
            };
            (start < end).then(|| TextRun {
                len: end - start,
                color,
                background_color,
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
        let theme = Theme::current(cx);
        let content_is_empty = content.is_empty();
        let (display_text, text_color, selected_range, marked_range) = if content_is_empty {
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
        let palette = crate::md::render::Palette::from_theme(&theme);
        let search = if content_is_empty {
            SearchPaint::none()
        } else {
            SearchPaint {
                matches: &input.search_matches,
                active: input
                    .active_search_match
                    .and_then(|index| input.search_matches.get(index)),
                match_color: theme.warning.opacity(0.22),
                active_color: theme.warning.opacity(0.5),
            }
        };
        let runs = input_text_runs(
            display_text.len(),
            base_run,
            selected_range,
            marked_range,
            theme.inverse.opacity(0.18),
            if content_is_empty {
                &[]
            } else {
                &input.highlight
            },
            |class| palette.token(class),
            search,
        );
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
        let theme = Theme::current(cx);
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
        let theme = Theme::current(cx);
        let input = cx.entity();
        let context_menu_input = input.clone();
        let field = div()
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
            .on_action(cx.listener(Self::move_to_previous_word))
            .on_action(cx.listener(Self::move_to_next_word))
            .on_action(cx.listener(Self::select_to_start))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::select_to_previous_word))
            .on_action(cx.listener(Self::select_to_next_word))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::delete_to_end))
            .on_action(cx.listener(Self::delete_to_previous_word))
            .on_action(cx.listener(Self::delete_to_next_word))
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
            .text_color(theme.text)
            // A composer owns its own metrics; a code editor inherits the
            // caller's, so a gutter beside it can rely on the same line height.
            .when(self.mode == FieldMode::Composer, |field| {
                field
                    .min_h(px(24.0))
                    .line_height(px(22.0))
                    .text_size(px(13.5))
            })
            .child(InputElement { input });

        context_menu(
            div().w_full().child(field),
            "composer-context-menu",
            &self.context_menu,
            move |cx| {
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

                // Call the editing methods directly rather than dispatching the
                // actions: by the time an item runs, focus is still unwinding
                // from the menu card, so a dispatch would have nowhere to land.
                let run = |input: &Entity<ComposerInput>,
                           action: fn(
                    &mut ComposerInput,
                    &mut Window,
                    &mut Context<ComposerInput>,
                )| {
                    let input = input.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let focus = input.read(cx).focus_handle.clone();
                        window.focus(&focus, cx);
                        input.update(cx, |input, cx| action(input, window, cx));
                    }
                };

                vec![
                    MenuItem::new(
                        "Cut",
                        run(&context_menu_input, |input, window, cx| {
                            input.cut(&Cut, window, cx)
                        }),
                    )
                    .disabled(!has_selection),
                    MenuItem::new(
                        "Copy",
                        run(&context_menu_input, |input, window, cx| {
                            input.copy(&Copy, window, cx)
                        }),
                    )
                    .disabled(!has_selection),
                    MenuItem::new(
                        "Paste",
                        run(&context_menu_input, |input, window, cx| {
                            input.paste(&Paste, window, cx)
                        }),
                    )
                    .disabled(!can_paste),
                    MenuItem::Separator,
                    MenuItem::new(
                        "Select All",
                        run(&context_menu_input, |input, window, cx| {
                            input.select_all(&SelectAll, window, cx)
                        }),
                    )
                    .disabled(!has_content || all_selected),
                ]
            },
        )
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

    use super::TokenClass;
    use super::{
        SearchPaint, cursor_should_be_visible, input_text_runs, next_word_boundary,
        previous_word_boundary, word_range_at,
    };

    #[test]
    fn word_navigation_matches_native_text_inputs() {
        let text = "hello, world  👋";

        assert_eq!(next_word_boundary(text, 0), 5);
        assert_eq!(next_word_boundary(text, 5), 6);
        assert_eq!(next_word_boundary(text, 6), 12);
        assert_eq!(next_word_boundary(text, 12), text.len());

        assert_eq!(previous_word_boundary(text, text.len()), 14);
        assert_eq!(previous_word_boundary(text, 14), 7);
        assert_eq!(previous_word_boundary(text, 7), 5);
        assert_eq!(previous_word_boundary(text, 5), 0);
    }

    #[test]
    fn double_click_ranges_follow_unicode_word_boundaries() {
        let text = "hello,  world 👋";

        assert_eq!(word_range_at(text, 1), 0..5);
        assert_eq!(word_range_at(text, 5), 5..6);
        assert_eq!(word_range_at(text, 6), 6..8);
        assert_eq!(word_range_at(text, 9), 8..13);
        assert_eq!(word_range_at(text, 14), 14..text.len());
        assert_eq!(word_range_at(text, text.len()), text.len()..text.len());
    }

    #[test]
    fn context_menu_keeps_cursor_visible_while_it_owns_focus() {
        assert!(cursor_should_be_visible(true, false, true, false));
        assert!(!cursor_should_be_visible(true, false, false, true));
        assert!(!cursor_should_be_visible(false, false, true, true));
    }

    /// Syntax colours and the selection wash are independent layers over the
    /// same text, so their boundaries have to interleave without either losing
    /// coverage — the runs must still tile the content exactly.
    #[test]
    fn syntax_colours_and_selection_split_into_tiling_runs() {
        let selection = 4..12;
        let keyword = hsla(0.8, 0.5, 0.6, 1.0);
        let plain = hsla(0.0, 0.0, 1.0, 1.0);
        // "let" at 0..3 and "true" at 8..12, with a selection cutting across.
        let highlight = vec![(0..3, TokenClass::Keyword), (8..12, TokenClass::Literal)];
        let runs = input_text_runs(
            12,
            TextRun {
                len: 12,
                font: font(".SystemUIFont"),
                color: plain,
                background_color: None,
                underline: None,
                strikethrough: None,
            },
            Some(&selection),
            None,
            hsla(0.0, 0.0, 1.0, 0.18),
            &highlight,
            |class| match class {
                TokenClass::Keyword => keyword,
                _ => plain,
            },
            SearchPaint::none(),
        );

        assert_eq!(
            runs.iter().map(|run| run.len).sum::<usize>(),
            12,
            "runs must tile the content: {runs:?}"
        );
        // The keyword keeps its colour and stays outside the selection.
        assert_eq!(runs[0].len, 3);
        assert_eq!(runs[0].color, keyword);
        assert!(runs[0].background_color.is_none());
        // Everything inside the selection carries the wash.
        let selected_len: usize = runs
            .iter()
            .filter(|run| run.background_color.is_some())
            .map(|run| run.len)
            .sum();
        assert_eq!(selected_len, selection.len());
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
            hsla(0.0, 0.0, 1.0, 0.18),
            &[],
            |_| hsla(0.0, 0.0, 1.0, 1.0),
            SearchPaint::none(),
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

    /// Find washes tile with everything else, and the active match keeps its
    /// own colour even while it is also the selection — otherwise navigating
    /// (which selects the match) would make the current match look like any
    /// drag-selection.
    #[test]
    fn search_match_washes_layer_under_selection_except_the_active_one() {
        let plain = hsla(0.0, 0.0, 1.0, 1.0);
        let selection_color = hsla(0.6, 1.0, 0.5, 0.4);
        let match_color = hsla(0.1, 1.0, 0.5, 0.2);
        let active_color = hsla(0.1, 1.0, 0.5, 0.5);
        let matches = vec![2..4, 8..10, 14..16];
        let active = 8..10;
        // Selection covers the active match exactly, as after find-next.
        let selection = 8..10;
        let runs = input_text_runs(
            20,
            TextRun {
                len: 20,
                font: font(".SystemUIFont"),
                color: plain,
                background_color: None,
                underline: None,
                strikethrough: None,
            },
            Some(&selection),
            None,
            selection_color,
            &[],
            |_| plain,
            SearchPaint {
                matches: &matches,
                active: Some(&active),
                match_color,
                active_color,
            },
        );

        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), 20);
        let background_at = |offset: usize| {
            let mut cursor = 0;
            runs.iter()
                .find_map(|run| {
                    let range = cursor..cursor + run.len;
                    cursor += run.len;
                    range.contains(&offset).then_some(run.background_color)
                })
                .unwrap()
        };
        assert_eq!(background_at(2), Some(match_color));
        assert_eq!(background_at(8), Some(active_color));
        assert_eq!(background_at(14), Some(match_color));
        assert_eq!(background_at(6), None);
    }
}
