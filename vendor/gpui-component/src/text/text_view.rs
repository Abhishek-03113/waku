use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    Entity, EntityId, FocusHandle, GlobalElementId, Half, Hitbox, HitboxBehavior, InspectorElementId,
    InteractiveElement, IntoElement, KeyBinding, LayoutId, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, RenderOnce, SharedString, Size,
    StyleRefinement, Styled, TextLayout, Timer, Window, div, px,
};
use smol::stream::StreamExt;
use unicode_segmentation::UnicodeSegmentation;

use crate::highlighter::HighlightTheme;
use crate::scroll::ScrollableElement;
use crate::text::node::CodeBlock;
use crate::{ActiveTheme, StyledExt, v_flex};
use crate::{
    global_state::GlobalState,
    input::{self},
    text::{
        TextViewStyle,
        node::{self, NodeContext},
    },
};

const CONTEXT: &'static str = "TextView";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(vec![
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", input::Copy, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", input::Copy, Some(CONTEXT)),
    ]);
}

#[derive(IntoElement, Clone)]
struct TextViewElement {
    list_state: Option<ListState>,
    block_viewport: Option<TextViewScrollViewport>,
    block_resize_handler: Option<Arc<BlockResizeFn>>,
    state: Entity<TextViewState>,
}

impl RenderOnce for TextViewElement {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let measure_state = self.state.clone();
        self.state.update(cx, |state, cx| {
            let block_count = state
                .parsed_result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map_or(0, |content| content.root_node.root_block_count());
            let block_virtualization = self.block_viewport.as_ref().and_then(|viewport| {
                let mut virtualization = state.block_virtualization(
                    *viewport,
                    block_count,
                    measure_state.clone(),
                    self.block_resize_handler.clone(),
                )?;
                if state.is_selecting || state.selection_pointer.is_some() {
                    state.rendered_block_count.set(block_count);
                    virtualization.render_all = true;
                }
                Some(virtualization)
            });

            v_flex().size_full().map(|this| match &state.parsed_result {
                Some(Ok(content)) => this.child(content.root_node.render_root(
                    self.list_state.clone(),
                    block_virtualization,
                    &content.node_cx,
                    window,
                    cx,
                )),
                Some(Err(err)) => this.child(
                    v_flex()
                        .gap_1()
                        .child("Failed to parse content")
                        .child(err.to_string()),
                ),
                None => this,
            })
        })
    }
}

/// Type for code block actions generator function.
pub(crate) type CodeBlockActionsFn =
    dyn Fn(&CodeBlock, &mut Window, &mut App) -> AnyElement + Send + Sync;

/// A top-level Markdown block changed height after it had already been laid
/// out. Initial measurements are deliberately excluded: consumers use this
/// signal for real runtime changes (for example an image finishing loading or
/// a disclosure opening), not for ordinary virtualization discovery.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextViewBlockResize {
    /// Index in the parsed document's top-level block list.
    pub block_index: usize,
    /// New height minus the previously measured height.
    pub delta: Pixels,
    /// Whether the old block ended above the visible viewport. Consumers can
    /// compensate their scroll anchor by `delta` in this case.
    pub above_viewport: bool,
}

pub(crate) type BlockResizeFn = dyn Fn(TextViewBlockResize, &mut App) + Send + Sync;

/// A text view that can render Markdown or HTML.
///
/// ## Goals
///
/// - Provide a rich text rendering component for such as Markdown or HTML,
/// used to display rich text in GPUI application (e.g., Help messages, Release notes)
/// - Support Markdown GFM and HTML (Simple HTML like Safari Reader Mode) for showing most common used markups.
/// - Support Heading, Paragraph, Bold, Italic, StrikeThrough, Code, Link, Image, Blockquote, List, Table, HorizontalRule, CodeBlock ...
///
/// ## Not Goals
///
/// - Customization of the complex style (some simple styles will be supported)
/// - As a Markdown editor or viewer (If you want to like this, you must fork your version).
/// - As a HTML viewer, we not support CSS, we only support basic HTML tags for used to as a content reader.
///
/// See also [`MarkdownElement`], [`HtmlElement`]
#[derive(Clone)]
pub struct TextView {
    id: ElementId,
    init_state: Option<InitState>,
    raw: SharedString,
    state: Entity<TextViewState>,
    style: StyleRefinement,
    selectable: bool,
    scrollable: bool,
    selection_scroll_handle: Option<ListState>,
    block_viewport: Option<TextViewScrollViewport>,
    block_layout_width: Option<Pixels>,
    block_resize_handler: Option<Arc<BlockResizeFn>>,
    update_delay: Duration,
    code_block_actions: Option<Arc<CodeBlockActionsFn>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TextViewScrollViewport {
    bounds: Bounds<Pixels>,
    scroll_offset: Pixels,
}

impl TextViewScrollViewport {
    pub fn from_list(list_state: &ListState) -> Self {
        Self {
            bounds: list_state.viewport_bounds(),
            scroll_offset: list_state.scroll_px_offset_for_scrollbar().y,
        }
    }
}

fn text_interaction_bounds(
    bounds: Bounds<Pixels>,
    viewport: Option<TextViewScrollViewport>,
) -> Bounds<Pixels> {
    viewport.map_or(bounds, |viewport| bounds.intersect(&viewport.bounds))
}

#[derive(PartialEq)]
pub(crate) struct ParsedContent {
    pub(crate) root_node: node::Node,
    pub(crate) node_cx: node::NodeContext,
}

fn matching_root_block_prefix(
    current: Option<&Result<ParsedContent, SharedString>>,
    next: &Result<ParsedContent, SharedString>,
) -> usize {
    let (Some(Ok(current)), Ok(next)) = (current, next) else {
        return 0;
    };
    if current.node_cx != next.node_cx {
        return 0;
    }

    current
        .root_node
        .root_blocks()
        .iter()
        .zip(next.root_node.root_blocks())
        .take_while(|(current, next)| current == next)
        .count()
}

/// The type of the text view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextViewType {
    /// Markdown view
    Markdown,
    /// HTML view
    Html,
}

enum Update {
    Text(SharedString),
    Style(Box<TextViewStyle>),
}

struct UpdateFuture {
    type_: TextViewType,
    highlight_theme: Arc<HighlightTheme>,
    current_style: TextViewStyle,
    current_text: SharedString,
    timer: Timer,
    timer_pending: bool,
    rx: Pin<Box<smol::channel::Receiver<Update>>>,
    tx_result: smol::channel::Sender<Result<ParsedContent, SharedString>>,
    delay: Duration,
    code_block_actions: Option<Arc<CodeBlockActionsFn>>,
}

impl UpdateFuture {
    #[allow(clippy::too_many_arguments)]
    fn new(
        type_: TextViewType,
        style: TextViewStyle,
        text: SharedString,
        highlight_theme: Arc<HighlightTheme>,
        rx: smol::channel::Receiver<Update>,
        tx_result: smol::channel::Sender<Result<ParsedContent, SharedString>>,
        delay: Duration,
        code_block_actions: Option<Arc<CodeBlockActionsFn>>,
    ) -> Self {
        Self {
            type_,
            highlight_theme,
            current_style: style,
            current_text: text,
            timer: Timer::never(),
            timer_pending: false,
            rx: Box::pin(rx),
            tx_result,
            delay,
            code_block_actions,
        }
    }
}

impl Future for UpdateFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.rx.poll_next(cx) {
                Poll::Ready(Some(update)) => {
                    let changed = match update {
                        Update::Text(text) if self.current_text != text => {
                            self.current_text = text;
                            true
                        }
                        Update::Style(style) if self.current_style != *style => {
                            self.current_style = *style;
                            true
                        }
                        _ => false,
                    };
                    if changed && !self.timer_pending {
                        let delay = self.delay;
                        self.timer.set_after(delay);
                        self.timer_pending = true;
                    }
                    continue;
                }
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => {}
            }

            match self.timer.poll_next(cx) {
                Poll::Ready(Some(_)) => {
                    self.timer_pending = false;
                    let res = parse_content(
                        self.type_,
                        &self.current_text,
                        self.current_style.clone(),
                        &self.highlight_theme,
                        &self.code_block_actions.clone(),
                    );
                    _ = self.tx_result.try_send(res);
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Clone)]
enum InitState {
    Initializing {
        type_: TextViewType,
        text: SharedString,
        style: Box<TextViewStyle>,
        highlight_theme: Arc<HighlightTheme>,
    },
    Initialized {
        tx: smol::channel::Sender<Update>,
        current_style: TextViewStyle,
    },
}

pub struct TextViewState {
    parent_entity: Option<EntityId>,
    tx: Option<smol::channel::Sender<Update>>,
    parsed_result: Option<Result<ParsedContent, SharedString>>,
    focus_handle: Option<FocusHandle>,
    /// The bounds of the text view
    bounds: Bounds<Pixels>,
    /// The local (in TextView) position of the selection.
    selection_positions: (Option<Point<Pixels>>, Option<Point<Pixels>>),
    /// Current drag position in window coordinates. Keeping this lets a
    /// selection continue growing while its parent scrolls under the pointer.
    selection_pointer: Option<Point<Pixels>>,
    selection_autoscroll_active: bool,
    active_selection_scope: Option<usize>,
    /// Is current in selection.
    is_selecting: bool,
    is_selectable: bool,
    list_state: ListState,
    current_text: SharedString,
    current_style: TextViewStyle,
    block_heights: Vec<Option<Pixels>>,
    block_reported_deltas: Vec<Pixels>,
    block_rebase_pending: Vec<bool>,
    block_width: Pixels,
    block_resize_handler: Option<Arc<BlockResizeFn>>,
    parent_scroll_offset: Pixels,
    rendered_block_count: Rc<Cell<usize>>,
    selectable_text: Vec<SelectableText>,
}

#[derive(Clone)]
struct SelectableText {
    bounds: Bounds<Pixels>,
    text: SharedString,
    layout: TextLayout,
    selection_scope: Option<usize>,
}

impl TextViewState {
    pub fn new(cx: &mut Context<TextViewState>) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            parent_entity: None,
            tx: None,
            parsed_result: None,
            focus_handle: Some(focus_handle),
            bounds: Bounds::default(),
            selection_positions: (None, None),
            selection_pointer: None,
            selection_autoscroll_active: false,
            active_selection_scope: None,
            is_selecting: false,
            is_selectable: false,
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(1000.)),
            current_text: SharedString::default(),
            current_style: TextViewStyle::default(),
            block_heights: Vec::new(),
            block_reported_deltas: Vec::new(),
            block_rebase_pending: Vec::new(),
            block_width: Pixels::ZERO,
            block_resize_handler: None,
            parent_scroll_offset: Pixels::ZERO,
            rendered_block_count: Rc::new(Cell::new(0)),
            selectable_text: Vec::new(),
        }
    }
}

impl TextViewState {
    /// Forget geometry owned by the previous parent-list mount while retaining
    /// parsed content and measured Markdown block heights. A virtualized
    /// transcript can remount the same state after resetting its outer list;
    /// the old row bounds and scroll offset must not influence the first block
    /// window chosen for the new mount.
    pub fn reset_block_viewport_layout(&mut self) {
        self.bounds = Bounds::default();
        self.parent_scroll_offset = Pixels::ZERO;
    }

    fn invalidate_block_heights_for_width(&mut self) {
        self.block_rebase_pending
            .resize(self.block_reported_deltas.len(), false);
        for (pending, reported) in self
            .block_rebase_pending
            .iter_mut()
            .zip(&self.block_reported_deltas)
        {
            *pending = reported.abs() >= px(0.5);
        }
        self.block_heights.clear();
    }

    /// Save bounds and unselect only when width changes can reflow text.
    /// Height-only changes are normal while a variable-height parent list
    /// settles its measurements and do not invalidate document coordinates.
    fn update_bounds(&mut self, bounds: Bounds<Pixels>) {
        let width_changed = self.bounds.size.width > Pixels::ZERO
            && bounds.size.width > Pixels::ZERO
            && self.bounds.size.width != bounds.size.width;
        let width_was_prepared = self.block_width > Pixels::ZERO
            && (self.block_width - bounds.size.width).abs() < px(2.0);
        if width_changed && !width_was_prepared {
            self.invalidate_block_heights_for_width();
            if self.selection_pointer.is_none() {
                self.clear_selection();
            }
        }
        if bounds.size.width > Pixels::ZERO {
            self.block_width = bounds.size.width;
        }
        self.bounds = bounds;
    }

    fn prepare_block_layout_width(&mut self, width: Pixels) {
        if width <= Pixels::ZERO {
            return;
        }
        if self.block_width > Pixels::ZERO && (self.block_width - width).abs() >= px(2.0) {
            self.invalidate_block_heights_for_width();
            if self.selection_pointer.is_none() {
                self.clear_selection();
            }
        }
        self.block_width = width;
    }

    fn replace_parsed_result(
        &mut self,
        mut parsed_result: Result<ParsedContent, SharedString>,
    ) -> Option<TextViewBlockResize> {
        let retained = matching_root_block_prefix(self.parsed_result.as_ref(), &parsed_result);
        if let (Some(Ok(current)), Ok(next)) = (self.parsed_result.as_ref(), &mut parsed_result) {
            for (next, current) in next
                .root_node
                .root_blocks_mut()
                .iter_mut()
                .zip(current.root_node.root_blocks())
            {
                next.reuse_interaction_state_from(current);
            }
        }
        let block_count = parsed_result
            .as_ref()
            .ok()
            .map_or(0, |content| content.root_node.root_block_count());
        let removed_delta = self.block_reported_deltas
            [retained.min(self.block_reported_deltas.len())..]
            .iter()
            .copied()
            .fold(Pixels::ZERO, |total, delta| total + delta);
        self.block_heights.truncate(retained);
        self.block_heights.resize(block_count, None);
        self.block_reported_deltas.truncate(retained);
        self.block_reported_deltas.resize(block_count, Pixels::ZERO);
        self.block_rebase_pending.truncate(retained);
        self.block_rebase_pending.resize(block_count, false);
        self.parsed_result = Some(parsed_result);
        (removed_delta.abs() >= px(0.5)).then_some(TextViewBlockResize {
            block_index: retained,
            delta: -removed_delta,
            above_viewport: false,
        })
    }

    fn block_virtualization(
        &mut self,
        viewport: TextViewScrollViewport,
        block_count: usize,
        measure_state: Entity<TextViewState>,
        resize_handler: Option<Arc<BlockResizeFn>>,
    ) -> Option<node::BlockVirtualization> {
        if block_count == 0 {
            return None;
        }

        self.block_heights.resize(block_count, None);
        self.block_rebase_pending.resize(block_count, false);
        let content_width = if self.block_width > Pixels::ZERO {
            self.block_width
        } else if self.bounds.size.width > Pixels::ZERO {
            self.bounds.size.width
        } else {
            viewport.bounds.size.width
        };
        if self.bounds.size.width <= Pixels::ZERO {
            return Some(node::BlockVirtualization {
                viewport: viewport.bounds,
                // The row origin is not known until the first paint. Render a
                // bounded window from the start of the estimated document,
                // then request one follow-up frame with the real origin.
                content_origin_y: viewport.bounds.origin.y,
                content_width,
                heights: self.block_heights.clone(),
                measure_state,
                resize_handler,
                render_all: false,
                rendered_block_count: self.rendered_block_count.clone(),
            });
        }

        let scroll_offset = viewport.scroll_offset;
        let predicted_origin_y = self.bounds.origin.y + (scroll_offset - self.parent_scroll_offset);
        Some(node::BlockVirtualization {
            viewport: viewport.bounds,
            content_origin_y: predicted_origin_y,
            content_width,
            heights: self.block_heights.clone(),
            measure_state,
            resize_handler,
            render_all: false,
            rendered_block_count: self.rendered_block_count.clone(),
        })
    }

    pub(crate) fn record_block_height(
        &mut self,
        index: usize,
        height: Pixels,
        estimated_height: Pixels,
    ) -> Option<(Pixels, bool)> {
        if self.block_heights.len() <= index {
            self.block_heights.resize(index + 1, None);
        }
        let previous = self.block_heights[index];
        self.block_heights[index] = Some(height);
        if self
            .block_rebase_pending
            .get(index)
            .copied()
            .unwrap_or(false)
        {
            self.block_rebase_pending[index] = false;
            let previous_reported = self.block_reported_deltas[index];
            let next_reported = height - estimated_height;
            self.block_reported_deltas[index] = next_reported;
            let correction = next_reported - previous_reported;
            return (correction.abs() >= px(0.5)).then_some((correction, true));
        }
        let delta = previous
            .map(|previous| height - previous)
            .filter(|delta| delta.abs() >= px(0.5));
        if let Some(delta) = delta
            && self.block_resize_handler.is_some()
        {
            if self.block_reported_deltas.len() <= index {
                self.block_reported_deltas.resize(index + 1, Pixels::ZERO);
            }
            self.block_reported_deltas[index] += delta;
        }
        delta.map(|delta| (delta, false))
    }

    #[cfg(test)]
    fn rendered_block_count(&self) -> usize {
        self.rendered_block_count.get()
    }

    fn clear_selection(&mut self) {
        self.selection_positions = (None, None);
        self.selection_pointer = None;
        self.selection_autoscroll_active = false;
        self.active_selection_scope = None;
        self.is_selecting = false;
    }

    fn start_selection(&mut self, pos: Point<Pixels>) {
        // Pointer drags are document selections, even when they begin inside
        // a Markdown table cell. Selection scopes are only needed for the
        // exact range produced by double- and triple-click selection.
        self.active_selection_scope = None;
        let pos = pos - self.bounds.origin;
        self.selection_positions = (Some(pos), Some(pos));
        self.selection_pointer = Some(pos + self.bounds.origin);
        self.selection_autoscroll_active = false;
        self.is_selecting = true;
    }

    fn select_text_between(
        &mut self,
        start: Point<Pixels>,
        end: Point<Pixels>,
        selection_scope: Option<usize>,
    ) {
        self.selection_positions = (
            Some(start - self.bounds.origin),
            Some(end - self.bounds.origin),
        );
        self.selection_pointer = None;
        self.selection_autoscroll_active = false;
        self.active_selection_scope = selection_scope;
        self.is_selecting = false;
    }

    pub(crate) fn record_selectable_text(
        &mut self,
        bounds: Bounds<Pixels>,
        text: SharedString,
        layout: TextLayout,
        selection_scope: Option<usize>,
    ) {
        if self.is_selectable {
            self.selectable_text.push(SelectableText {
                bounds,
                text,
                layout,
                selection_scope,
            });
        }
    }

    pub(crate) fn selection_allows_scope(&self, selection_scope: Option<usize>) -> bool {
        self.active_selection_scope
            .is_none_or(|active_scope| selection_scope == Some(active_scope))
    }

    fn select_text_at(&mut self, position: Point<Pixels>, click_count: usize) -> bool {
        let (mut start, mut end, selection_scope, line_height) = {
            let Some(text) = self
                .selectable_text
                .iter()
                .rev()
                .find(|text| text.bounds.contains(&position))
            else {
                return false;
            };
            let index = match text.layout.index_for_position(position) {
                Ok(index) | Err(index) => index,
            };
            let range = if click_count == 2 {
                surrounding_word_range(&text.text, index)
            } else {
                surrounding_line_range(&text.text, index)
            };
            let Some(start) = text.layout.position_for_index(range.start) else {
                return false;
            };
            let Some(end) = text.layout.position_for_index(range.end) else {
                return false;
            };
            (start, end, text.selection_scope, text.layout.line_height())
        };
        // Text positions sit on the top edge of a line. Put the selection
        // endpoints inside that line so the geometric selection resolver does
        // not also include glyphs from the adjacent wrapped line.
        let line_center = line_height.half();
        start.y += line_center;
        end.y += line_center + px(1.0);

        self.select_text_between(start, end, selection_scope);
        true
    }

    fn update_selection(&mut self, pos: Point<Pixels>) {
        self.selection_pointer = Some(pos);
        self.update_selection_from_pointer();
    }

    fn update_selection_from_pointer(&mut self) {
        let Some(pointer) = self.selection_pointer else {
            return;
        };
        let pos = pointer - self.bounds.origin;
        if let (Some(start), Some(_)) = self.selection_positions {
            self.selection_positions = (Some(start), Some(pos))
        }
    }

    fn advance_selection_for_scroll(&mut self, delta: Pixels) {
        if let (Some(start), Some(mut end)) = self.selection_positions {
            end.y += delta;
            self.selection_positions = (Some(start), Some(end));
        }
    }

    fn end_selection(&mut self) {
        self.selection_autoscroll_active = false;
        self.is_selecting = false;
    }

    pub(crate) fn has_selection(&self) -> bool {
        if let (Some(start), Some(end)) = self.selection_positions {
            start != end
        } else {
            false
        }
    }

    pub(crate) fn is_selectable(&self) -> bool {
        self.is_selectable
    }

    /// Return the bounds of the selection in window coordinates.
    pub(crate) fn selection_bounds(&self) -> Bounds<Pixels> {
        selection_bounds(
            self.selection_positions.0,
            self.selection_positions.1,
            self.bounds,
        )
    }

    fn selection_text(&self) -> Option<String> {
        Some(
            self.parsed_result
                .as_ref()?
                .as_ref()
                .ok()?
                .root_node
                .selected_text(),
        )
    }
}

#[derive(IntoElement, Clone)]
pub enum Text {
    String(SharedString),
    TextView(Box<TextView>),
}

impl From<SharedString> for Text {
    fn from(s: SharedString) -> Self {
        Self::String(s)
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Self::String(SharedString::from(s.to_string()))
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Self::String(s.into())
    }
}

impl From<TextView> for Text {
    fn from(e: TextView) -> Self {
        Self::TextView(Box::new(e))
    }
}

impl Text {
    /// Set the style for [`TextView`].
    ///
    /// Do nothing if this is `String`.
    pub fn style(self, style: TextViewStyle) -> Self {
        match self {
            Self::String(s) => Self::String(s),
            Self::TextView(e) => Self::TextView(Box::new(e.style(style))),
        }
    }

    /// Get the str
    pub fn as_str(&self) -> &str {
        match self {
            Self::String(s) => s.as_str(),
            Self::TextView(view) => view.raw.as_str(),
        }
    }
}

impl RenderOnce for Text {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        match self {
            Self::String(s) => s.into_any_element(),
            Self::TextView(e) => e.into_any_element(),
        }
    }
}

impl Styled for TextView {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl TextView {
    fn create_init_state(
        type_: TextViewType,
        text: &SharedString,
        highlight_theme: &Arc<HighlightTheme>,
        state: &Entity<TextViewState>,
        cx: &mut App,
    ) -> InitState {
        let state = state.read(cx);
        if let Some(tx) = &state.tx {
            InitState::Initialized {
                tx: tx.clone(),
                current_style: state.current_style.clone(),
            }
        } else {
            InitState::Initializing {
                type_,
                text: text.clone(),
                style: Default::default(),
                highlight_theme: highlight_theme.clone(),
            }
        }
    }

    /// Create a new markdown text view.
    pub fn markdown(
        id: impl Into<ElementId>,
        markdown: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let markdown = markdown.into();
        let state =
            window.use_keyed_state(SharedString::from(format!("{}/state", id)), cx, |_, cx| {
                TextViewState::new(cx)
            });
        Self::markdown_with_state(id, markdown, state, cx)
    }

    /// Create a Markdown view backed by state owned outside the element tree.
    /// This keeps selection and parsed content alive when a virtualized parent
    /// temporarily remounts the view while scrolling.
    pub fn markdown_with_state(
        id: impl Into<ElementId>,
        markdown: impl Into<SharedString>,
        state: Entity<TextViewState>,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let markdown = markdown.into();
        let highlight_theme = cx.theme().highlight_theme.clone();
        let init_state = Self::create_init_state(
            TextViewType::Markdown,
            &markdown,
            &highlight_theme,
            &state,
            cx,
        );
        if let Some(tx) = state.read(cx).tx.clone() {
            let changed = state.update(cx, |state, _| {
                if state.current_text == markdown {
                    false
                } else {
                    state.current_text = markdown.clone();
                    true
                }
            });
            if changed {
                let _ = tx.try_send(Update::Text(markdown.clone()));
            }
        }
        Self {
            id,
            init_state: Some(init_state),
            raw: markdown.clone(),
            style: StyleRefinement::default(),
            state,
            selectable: false,
            scrollable: false,
            selection_scroll_handle: None,
            block_viewport: None,
            block_layout_width: None,
            block_resize_handler: None,
            update_delay: Duration::from_millis(200),
            code_block_actions: None,
        }
    }

    /// Create a new html text view.
    pub fn html(
        id: impl Into<ElementId>,
        html: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let html = html.into();
        let state =
            window.use_keyed_state(SharedString::from(format!("{}/state", id)), cx, |_, cx| {
                TextViewState::new(cx)
            });
        Self::html_with_state(id, html, state, cx)
    }

    /// Create an HTML view backed by state owned outside the element tree.
    pub fn html_with_state(
        id: impl Into<ElementId>,
        html: impl Into<SharedString>,
        state: Entity<TextViewState>,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let html = html.into();
        let highlight_theme = cx.theme().highlight_theme.clone();
        let init_state =
            Self::create_init_state(TextViewType::Html, &html, &highlight_theme, &state, cx);
        if let Some(tx) = state.read(cx).tx.clone() {
            let changed = state.update(cx, |state, _| {
                if state.current_text == html {
                    false
                } else {
                    state.current_text = html.clone();
                    true
                }
            });
            if changed {
                let _ = tx.try_send(Update::Text(html.clone()));
            }
        }
        Self {
            id,
            init_state: Some(init_state),
            style: StyleRefinement::default(),
            state,
            raw: html,
            selectable: false,
            scrollable: false,
            selection_scroll_handle: None,
            block_viewport: None,
            block_layout_width: None,
            block_resize_handler: None,
            update_delay: Duration::from_millis(200),
            code_block_actions: None,
        }
    }

    /// Set the source text of the text view.
    pub fn text(mut self, raw: impl Into<SharedString>) -> Self {
        let raw: SharedString = raw.into();
        if let Some(init_state) = &mut self.init_state {
            match init_state {
                InitState::Initializing { text, .. } => *text = raw.clone(),
                InitState::Initialized { tx, .. } => {
                    let _ = tx.try_send(Update::Text(raw.clone()));
                }
            }
        }
        self.raw = raw;
        self
    }

    /// Set [`TextViewStyle`].
    pub fn style(mut self, style: TextViewStyle) -> Self {
        if let Some(init_state) = &mut self.init_state {
            match init_state {
                InitState::Initializing { style: s, .. } => **s = style,
                InitState::Initialized { tx, current_style } => {
                    if *current_style != style {
                        *current_style = style.clone();
                        let _ = tx.try_send(Update::Style(Box::new(style)));
                    }
                }
            }
        }
        self
    }

    /// Set the text view to be selectable, default is false.
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Scroll this parent list while dragging a text selection near or beyond
    /// its viewport edge.
    pub fn selection_scroll_handle(mut self, scroll_handle: &ListState) -> Self {
        self.selection_scroll_handle = Some(scroll_handle.clone());
        self
    }

    /// Provide the parent transcript viewport captured before its list starts
    /// layout. This lets Markdown virtualize blocks without re-borrowing the
    /// parent `ListState` from inside a row render.
    pub fn block_viewport(mut self, viewport: TextViewScrollViewport) -> Self {
        self.block_viewport = Some(viewport);
        self
    }

    /// Supply the width known by a virtualized parent before layout starts.
    /// This invalidates stale wrap-dependent block heights before the visible
    /// range is chosen, instead of one frame after the resize.
    pub fn block_layout_width(mut self, width: Pixels) -> Self {
        self.block_layout_width = Some(width);
        self
    }

    /// Observe real post-layout height changes in a top-level Markdown block.
    /// Virtualized parents use this to invalidate their cached row and keep
    /// the scrollbar and visible-content anchor coherent.
    pub fn on_block_resize(
        mut self,
        handler: impl Fn(TextViewBlockResize, &mut App) + Send + Sync + 'static,
    ) -> Self {
        self.block_resize_handler = Some(Arc::new(handler));
        self
    }

    /// Set the text view to be scrollable, default is false.
    ///
    /// ## If true for `scrollable`
    ///
    /// The `scrollable` mode used for large content,
    /// will show scrollbar, but requires the parent to have a fixed height,
    /// and use [`gpui::list`] to render the content in a virtualized way.
    ///
    /// ## If false to fit content
    ///
    /// The TextView will expand to fit all content, no scrollbar.
    /// This mode is suitable for small content, such as a few lines of text, a label, etc.
    pub fn scrollable(mut self, scrollable: bool) -> Self {
        self.scrollable = scrollable;
        self
    }

    /// Set the debounce delay used when reparsing updated source text.
    ///
    /// The default is 200ms. Live content can use a shorter delay while
    /// retaining a stable element ID and its keyed view state.
    pub fn update_delay(mut self, delay: Duration) -> Self {
        self.update_delay = delay;
        self
    }

    fn on_action_copy(state: &Entity<TextViewState>, cx: &mut App) {
        let Some(selected_text) = state.read(cx).selection_text() else {
            return;
        };

        cx.write_to_clipboard(ClipboardItem::new_string(selected_text.trim().to_string()));
    }

    /// Set custom block actions for code blocks.
    ///
    /// The closure receives the [`CodeBlock`],
    /// and returns an element to display.
    pub fn code_block_actions<F, E>(mut self, f: F) -> Self
    where
        F: Fn(&CodeBlock, &mut Window, &mut App) -> E + Send + Sync + 'static,
        E: IntoElement,
    {
        self.code_block_actions = Some(Arc::new(move |code_block, window, cx| {
            f(&code_block, window, cx).into_any_element()
        }));
        self
    }
}

impl IntoElement for TextView {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextView {
    type RequestLayoutState = AnyElement;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.state.update(cx, |state, _| {
            state.block_resize_handler = self.block_resize_handler.clone();
        });
        if let Some(width) = self.block_layout_width {
            self.state
                .update(cx, |state, _| state.prepare_block_layout_width(width));
        }
        if let Some(InitState::Initializing {
            type_,
            text,
            style,
            highlight_theme,
        }) = self.init_state.take()
        {
            let style = *style;
            let highlight_theme = highlight_theme.clone();
            let code_block_actions = self.code_block_actions.clone();
            let (tx, rx) = smol::channel::unbounded::<Update>();
            let (tx_result, rx_result) =
                smol::channel::unbounded::<Result<ParsedContent, SharedString>>();
            let parsed_result = parse_content(
                type_,
                &text,
                style.clone(),
                &highlight_theme,
                &code_block_actions,
            );

            let removed_resize = self.state.update(cx, {
                let tx = tx.clone();
                |state, _| {
                    let removed_resize = state.replace_parsed_result(parsed_result);
                    state.tx = Some(tx);
                    state.current_text = text.clone();
                    state.current_style = style.clone();
                    removed_resize
                }
            });
            if let (Some(resize), Some(handler)) =
                (removed_resize, self.block_resize_handler.as_ref())
            {
                handler(resize, cx);
            }

            cx.spawn({
                let state = self.state.downgrade();
                async move |cx| {
                    while let Ok(parsed_result) = rx_result.recv().await {
                        if let Some(state) = state.upgrade() {
                            _ = state.update(cx, |state, cx| {
                                let removed_resize = state.replace_parsed_result(parsed_result);
                                if let (Some(resize), Some(handler)) =
                                    (removed_resize, state.block_resize_handler.as_ref())
                                {
                                    handler(resize, &mut **cx);
                                }
                                if let Some(parent_entity) = state.parent_entity {
                                    let app = &mut **cx;
                                    app.notify(parent_entity);
                                }
                                state.clear_selection();
                            });
                        } else {
                            // state released, stopping processing
                            break;
                        }
                    }
                }
            })
            .detach();

            cx.background_spawn(UpdateFuture::new(
                type_,
                style,
                text,
                highlight_theme,
                rx,
                tx_result,
                self.update_delay,
                code_block_actions,
            ))
            .detach();

            self.init_state = Some(InitState::Initialized {
                tx,
                current_style: self.state.read(cx).current_style.clone(),
            });
        }

        let list_state = &self.state.read(cx).list_state;

        let focus_handle = self
            .state
            .read(cx)
            .focus_handle
            .as_ref()
            .expect("focus_handle should init by TextViewState::new");

        let mut el = div()
            .key_context(CONTEXT)
            .track_focus(focus_handle)
            .size_full()
            .relative()
            .on_action({
                let state = self.state.clone();
                move |_: &input::Copy, _, cx| {
                    Self::on_action_copy(&state, cx);
                }
            })
            .child(TextViewElement {
                list_state: if self.scrollable {
                    Some(list_state.clone())
                } else {
                    None
                },
                block_viewport: self.block_viewport,
                block_resize_handler: self.block_resize_handler.clone(),
                state: self.state.clone(),
            })
            .refine_style(&self.style)
            .vertical_scrollbar(list_state)
            .into_any_element();
        let layout_id = el.request_layout(window, cx);
        (layout_id, el)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(
            text_interaction_bounds(bounds, self.block_viewport),
            HitboxBehavior::Normal,
        );
        request_layout.prepaint(window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let entity_id = window.current_view();
        let is_selectable = self.selectable;
        let interaction_bounds = text_interaction_bounds(bounds, self.block_viewport);

        if is_selectable {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }

        let (selection_settled, first_layout) = self.state.update(cx, |state, _| {
            let first_layout =
                state.bounds.size.width <= Pixels::ZERO && bounds.size.width > Pixels::ZERO;
            state.parent_entity = Some(entity_id);
            state.parent_scroll_offset = self
                .selection_scroll_handle
                .as_ref()
                .map_or(Pixels::ZERO, |handle| {
                    handle.scroll_px_offset_for_scrollbar().y
                });
            state.update_bounds(bounds);
            state.selectable_text.clear();
            let mut selection_settled = false;
            if state.selection_pointer.is_some() {
                state.update_selection_from_pointer();
                if !state.is_selecting {
                    // Mouse-up may arrive before the list paints its final
                    // autoscroll offset. Retain the pointer until this paint,
                    // then keep only the settled document-local endpoint.
                    state.selection_pointer = None;
                    selection_settled = true;
                }
            }
            state.is_selectable = is_selectable;
            (selection_settled, first_layout)
        });
        if selection_settled || first_layout {
            // Mouse-up renders the complete document once so the final
            // selection endpoint is accurate. A first layout also needs one
            // follow-up now that the row's real transcript origin is known.
            cx.notify(entity_id);
        }

        GlobalState::global_mut(cx)
            .text_view_state_stack
            .push(self.state.clone());
        request_layout.paint(window, cx);
        GlobalState::global_mut(cx).text_view_state_stack.pop();

        if self.selectable {
            let is_selecting = self.state.read(cx).is_selecting;
            let has_selection = self.state.read(cx).has_selection();

            if is_selecting
                && let Some(scroll_handle) = &self.selection_scroll_handle
                && let Some(pointer) = self.state.read(cx).selection_pointer
            {
                let delta = selection_autoscroll_delta(pointer.y, scroll_handle.viewport_bounds());
                if delta != Pixels::ZERO {
                    let scrolled = scroll_list_for_selection(scroll_handle, delta);
                    if scrolled != Pixels::ZERO {
                        self.state.update(cx, |state, _| {
                            state.advance_selection_for_scroll(scrolled);
                        });
                        window.request_animation_frame();
                    }
                }
            }

            window.on_mouse_event({
                let state = self.state.clone();
                move |event: &MouseDownEvent, phase, window, cx| {
                    if !interaction_bounds.contains(&event.position)
                        || !phase.bubble()
                        || event.button != MouseButton::Left
                        || window.default_prevented()
                    {
                        return;
                    }

                    let handled_multiple_click = state.update(cx, |state, _| {
                        if event.click_count >= 2
                            && state.select_text_at(event.position, event.click_count)
                        {
                            true
                        } else {
                            state.start_selection(event.position);
                            false
                        }
                    });
                    if handled_multiple_click {
                        window.prevent_default();
                    }
                    cx.notify(entity_id);
                }
            });

            // Keep these handlers installed before selection starts. A fast
            // drag can deliver down, move, and up before the next paint.
            window.on_mouse_event({
                let state = self.state.clone();
                let scroll_handle = self.selection_scroll_handle.clone();
                move |event: &MouseMoveEvent, _, _, cx| {
                    if !state.read(cx).is_selecting {
                        return;
                    }

                    let delta = scroll_handle
                        .as_ref()
                        .map_or(Pixels::ZERO, |scroll_handle| {
                            selection_autoscroll_delta(
                                event.position.y,
                                scroll_handle.viewport_bounds(),
                            )
                        });
                    let should_scroll_now = state.update(cx, |state, _| {
                        state.update_selection(event.position);
                        let should_scroll_now =
                            delta != Pixels::ZERO && !state.selection_autoscroll_active;
                        state.selection_autoscroll_active = delta != Pixels::ZERO;
                        should_scroll_now
                    });
                    if should_scroll_now && let Some(scroll_handle) = &scroll_handle {
                        let scrolled = scroll_list_for_selection(scroll_handle, delta);
                        if scrolled != Pixels::ZERO {
                            state.update(cx, |state, _| {
                                state.advance_selection_for_scroll(scrolled);
                            });
                        }
                    }
                    cx.notify(entity_id);
                }
            });

            window.on_mouse_event({
                let state = self.state.clone();
                move |event: &MouseUpEvent, _, _, cx| {
                    if event.button != MouseButton::Left || !state.read(cx).is_selecting {
                        return;
                    }

                    state.update(cx, |state, _| {
                        state.end_selection();
                    });
                    cx.notify(entity_id);
                }
            });

            if has_selection {
                // down outside to clear selection
                window.on_mouse_event({
                    let state = self.state.clone();
                    move |event: &MouseDownEvent, _, _, cx| {
                        if event.button != MouseButton::Left
                            || interaction_bounds.contains(&event.position)
                        {
                            return;
                        }

                        state.update(cx, |state, _| {
                            state.clear_selection();
                        });
                        cx.notify(entity_id);
                    }
                });
            }
        }
    }
}

fn surrounding_word_range(text: &str, index: usize) -> std::ops::Range<usize> {
    let index = index.min(text.len());
    let mut previous_non_whitespace = None;

    for (start, segment) in text.split_word_bound_indices() {
        let end = start + segment.len();
        let is_whitespace = segment.chars().all(char::is_whitespace);

        if start <= index && index < end {
            if is_whitespace {
                return previous_non_whitespace.unwrap_or(start..end);
            }
            return start..end;
        }

        if end <= index && !is_whitespace {
            previous_non_whitespace = Some(start..end);
        }
    }

    previous_non_whitespace.unwrap_or(index..index)
}

fn surrounding_line_range(text: &str, index: usize) -> std::ops::Range<usize> {
    let index = index.min(text.len());
    let start = text[..index].rfind('\n').map_or(0, |newline| newline + 1);
    let end = text[index..]
        .find('\n')
        .map_or(text.len(), |newline| index + newline);
    start..end
}

fn parse_content(
    type_: TextViewType,
    text: &str,
    style: TextViewStyle,
    highlight_theme: &HighlightTheme,
    code_block_actions: &Option<Arc<CodeBlockActionsFn>>,
) -> Result<ParsedContent, SharedString> {
    let mut node_cx = NodeContext {
        style: style.clone(),
        code_block_actions: code_block_actions.clone(),
        ..NodeContext::default()
    };

    let res = match type_ {
        TextViewType::Markdown => {
            super::format::markdown::parse(text, &style, &mut node_cx, highlight_theme)
        }
        TextViewType::Html => super::format::html::parse(text, &mut node_cx),
    };
    res.map(move |root_node| ParsedContent { root_node, node_cx })
}

fn selection_bounds(
    start: Option<Point<Pixels>>,
    end: Option<Point<Pixels>>,
    bounds: Bounds<Pixels>,
) -> Bounds<Pixels> {
    if let (Some(start), Some(end)) = (start, end) {
        let start = start + bounds.origin;
        let end = end + bounds.origin;

        let origin = Point {
            x: start.x.min(end.x),
            y: start.y.min(end.y),
        };
        let size = Size {
            width: (start.x - end.x).abs(),
            height: (start.y - end.y).abs(),
        };

        return Bounds { origin, size };
    }

    Bounds::default()
}

fn selection_autoscroll_delta(pointer_y: Pixels, viewport: Bounds<Pixels>) -> Pixels {
    if viewport.size.height <= Pixels::ZERO {
        return Pixels::ZERO;
    }

    const EDGE_SIZE: Pixels = px(32.0);
    const MIN_SPEED: Pixels = px(4.0);
    const MAX_SPEED: Pixels = px(36.0);

    let top_edge = viewport.top() + EDGE_SIZE;
    if pointer_y < top_edge {
        let strength = ((top_edge - pointer_y) / EDGE_SIZE).clamp(0.0, 1.0);
        return -(MIN_SPEED + (MAX_SPEED - MIN_SPEED) * strength);
    }

    let bottom_edge = viewport.bottom() - EDGE_SIZE;
    if pointer_y > bottom_edge {
        let strength = ((pointer_y - bottom_edge) / EDGE_SIZE).clamp(0.0, 1.0);
        return MIN_SPEED + (MAX_SPEED - MIN_SPEED) * strength;
    }

    Pixels::ZERO
}

fn scroll_list_for_selection(scroll_handle: &ListState, delta: Pixels) -> Pixels {
    const PINNED_TAIL_EPSILON: Pixels = px(0.5);

    let max_offset = scroll_handle.max_offset_for_scrollbar().height;
    let selection_max = (max_offset - PINNED_TAIL_EPSILON).max(Pixels::ZERO);
    let current =
        (-scroll_handle.scroll_px_offset_for_scrollbar().y).clamp(Pixels::ZERO, selection_max);
    let target = (current + delta).clamp(Pixels::ZERO, selection_max);
    scroll_handle.set_offset_from_scrollbar(Point::new(Pixels::ZERO, -target));
    target - current
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, ListAlignment, Modifiers, point, px, size};
    use std::fmt::Write as _;

    #[test]
    fn double_click_ranges_follow_unicode_word_boundaries() {
        let text = "Hello 世界 foo.bar";

        assert_eq!(surrounding_word_range(text, 2), 0..5);
        assert_eq!(surrounding_word_range(text, 6), 6..9);
        assert_eq!(surrounding_word_range(text, 14), 13..20);
        assert_eq!(surrounding_word_range(text, 16), 13..20);
        assert_eq!(surrounding_word_range(text, 17), 13..20);
    }

    #[test]
    fn triple_click_ranges_select_the_surrounding_logical_line() {
        let text = "first line\nsecond line\nthird line";

        assert_eq!(surrounding_line_range(text, 3), 0..10);
        assert_eq!(surrounding_line_range(text, 15), 11..22);
        assert_eq!(surrounding_line_range(text, text.len()), 23..33);
    }

    #[test]
    fn test_text_view_state_selection_bounds() {
        assert_eq!(
            selection_bounds(None, None, Default::default()),
            Bounds::default()
        );
        assert_eq!(
            selection_bounds(None, Some(point(px(10.), px(20.))), Default::default()),
            Bounds::default()
        );
        assert_eq!(
            selection_bounds(Some(point(px(10.), px(20.))), None, Default::default()),
            Bounds::default()
        );

        // 10,10 start
        //   |------|
        //   |      |
        //   |------|
        //         50,50
        assert_eq!(
            selection_bounds(
                Some(point(px(10.), px(10.))),
                Some(point(px(50.), px(50.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        // 10,10
        //   |------|
        //   |      |
        //   |------|
        //         50,50 start
        assert_eq!(
            selection_bounds(
                Some(point(px(50.), px(50.))),
                Some(point(px(10.), px(10.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        //        50,10 start
        //   |------|
        //   |      |
        //   |------|
        // 10,50
        assert_eq!(
            selection_bounds(
                Some(point(px(50.), px(10.))),
                Some(point(px(10.), px(50.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        //        50,10
        //   |------|
        //   |      |
        //   |------|
        // 10,50 start
        assert_eq!(
            selection_bounds(
                Some(point(px(10.), px(50.))),
                Some(point(px(50.), px(10.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
    }

    #[test]
    fn selection_autoscroll_uses_viewport_edges_and_direction() {
        let viewport = Bounds {
            origin: point(px(0.0), px(100.0)),
            size: size(px(500.0), px(400.0)),
        };

        assert_eq!(
            selection_autoscroll_delta(px(300.0), viewport),
            Pixels::ZERO
        );
        assert!(selection_autoscroll_delta(px(100.0), viewport) < Pixels::ZERO);
        assert!(selection_autoscroll_delta(px(500.0), viewport) > Pixels::ZERO);
        assert_eq!(selection_autoscroll_delta(px(0.0), viewport), -px(36.0));
        assert_eq!(selection_autoscroll_delta(px(600.0), viewport), px(36.0));
    }

    #[test]
    fn text_interactions_are_clipped_to_the_transcript_viewport() {
        let message_bounds = Bounds {
            origin: point(px(220.0), px(-1_000.0)),
            size: size(px(900.0), px(4_000.0)),
        };
        let transcript_bounds = Bounds {
            origin: point(px(220.0), px(40.0)),
            size: size(px(900.0), px(700.0)),
        };
        let interaction_bounds = text_interaction_bounds(
            message_bounds,
            Some(TextViewScrollViewport {
                bounds: transcript_bounds,
                scroll_offset: Pixels::ZERO,
            }),
        );

        assert_eq!(interaction_bounds, transcript_bounds);
        assert!(!interaction_bounds.contains(&point(px(700.0), px(20.0))));
        assert!(interaction_bounds.contains(&point(px(700.0), px(400.0))));
        assert_eq!(
            text_interaction_bounds(message_bounds, None),
            message_bounds
        );
    }

    #[gpui::test]
    fn secondary_mouse_down_preserves_an_existing_selection(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let text_state = cx.new(TextViewState::new);

        struct SelectionView {
            text_state: Entity<TextViewState>,
        }

        impl gpui::Render for SelectionView {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                TextView::markdown_with_state(
                    "secondary-click-selection",
                    "Selection must remain visible while its context menu opens.",
                    self.text_state.clone(),
                    cx,
                )
                .selectable(true)
                .w(px(600.0))
                .h(px(120.0))
            }
        }

        let (_view, cx) = cx.add_window_view(|_, _| SelectionView {
            text_state: text_state.clone(),
        });
        cx.update(|window, app| {
            window.activate_window();
            text_state.update(app, |state, _| {
                state.selection_positions = (
                    Some(point(px(12.0), px(8.0))),
                    Some(point(px(260.0), px(8.0))),
                );
                state.is_selecting = false;
                state.selection_pointer = None;
            });
            window.refresh();
        });
        cx.run_until_parked();

        let (click_position, outside_position, selection_before) = cx.update(|_, app| {
            let state = text_state.read(app);
            (
                state.bounds.origin + point(px(80.0), px(12.0)),
                point(
                    state.bounds.right() + px(10.0),
                    state.bounds.top() + px(12.0),
                ),
                state.selection_positions,
            )
        });
        cx.simulate_mouse_down(click_position, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_down(outside_position, MouseButton::Right, Modifiers::default());

        cx.update(|_, app| {
            let state = text_state.read(app);
            assert_eq!(state.selection_positions, selection_before);
            assert!(!state.is_selecting);
            assert!(state.has_selection());
        });
    }

    #[gpui::test]
    fn multiple_clicks_select_words_and_paragraphs(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let text_state = cx.new(TextViewState::new);

        struct SelectionView {
            text_state: Entity<TextViewState>,
        }

        impl gpui::Render for SelectionView {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                TextView::markdown_with_state(
                    "multiple-click-selection",
                    "Hello world from a selectable paragraph.",
                    self.text_state.clone(),
                    cx,
                )
                .selectable(true)
                .w(px(600.0))
                .h(px(120.0))
            }
        }

        let (_view, cx) = cx.add_window_view(|_, _| SelectionView {
            text_state: text_state.clone(),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        let click_position = cx.update(|_, app| {
            text_state
                .read(app)
                .selectable_text
                .first()
                .expect("paragraph text layout should be registered")
                .bounds
                .origin
                + point(px(18.0), px(10.0))
        });
        let selected = cx.update(|window, app| {
            let selected = text_state.update(app, |state, _| {
                state.select_text_at(click_position, 2)
            });
            window.refresh();
            selected
        });
        assert!(selected, "double click should resolve to an inline text range");
        cx.run_until_parked();
        cx.update(|_, app| {
            assert_eq!(
                text_state.read(app).selection_text().as_deref().map(str::trim),
                Some("Hello")
            );
        });

        let selected = cx.update(|window, app| {
            let selected = text_state.update(app, |state, _| {
                state.select_text_at(click_position, 3)
            });
            window.refresh();
            selected
        });
        assert!(selected, "triple click should resolve to an inline text range");
        cx.run_until_parked();
        cx.update(|_, app| {
            assert_eq!(
                text_state.read(app).selection_text().as_deref().map(str::trim),
                Some("Hello world from a selectable paragraph.")
            );
        });
    }

    #[gpui::test]
    fn double_click_selects_only_a_word_in_wrapped_text(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let text_state = cx.new(TextViewState::new);
        let text = "Luna wins on absolute coding/agentic capability — it's the only one with an independent (AA) coding-agent score, and every shared benchmark points its way. V4 Flash wins decisively on cost (~7x cheaper input, ~21x cheaper output), open weights, and long-context economics, and its 0731 agent scores are unverified vendor claims.";

        struct SelectionView {
            text: SharedString,
            text_state: Entity<TextViewState>,
        }

        impl gpui::Render for SelectionView {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                TextView::markdown_with_state(
                    "wrapped-multiple-click-selection",
                    self.text.clone(),
                    self.text_state.clone(),
                    cx,
                )
                .selectable(true)
                .w(px(800.0))
                .h(px(160.0))
            }
        }

        let (_view, cx) = cx.add_window_view(|_, _| SelectionView {
            text: text.into(),
            text_state: text_state.clone(),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        let click_position = cx.update(|_, app| {
            let selectable = text_state
                .read(app)
                .selectable_text
                .first()
                .expect("wrapped paragraph text layout should be registered");
            let index = selectable
                .text
                .find("economics")
                .expect("target word should be present")
                + 2;
            selectable
                .layout
                .position_for_index(index)
                .expect("target word should have a rendered position")
                + point(px(1.0), px(1.0))
        });
        let selected = cx.update(|window, app| {
            let selected = text_state.update(app, |state, _| {
                state.select_text_at(click_position, 2)
            });
            window.refresh();
            selected
        });
        assert!(selected, "double click should resolve to the wrapped word");
        cx.run_until_parked();
        cx.update(|_, app| {
            assert_eq!(
                text_state.read(app).selection_text().as_deref().map(str::trim),
                Some("economics")
            );
        });
    }

    #[gpui::test]
    fn table_drag_selection_crosses_cell_boundaries(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let text_state = cx.new(TextViewState::new);

        struct SelectionView {
            text_state: Entity<TextViewState>,
        }

        impl gpui::Render for SelectionView {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                TextView::markdown_with_state(
                    "table-cell-selection",
                    "| Label | Primary detail | Neighbor |\n| --- | --- | --- |\n| Score | Alpha cell content | Beta must be selectable |",
                    self.text_state.clone(),
                    cx,
                )
                .selectable(true)
                .w(px(720.0))
                .h(px(180.0))
            }
        }

        let (_view, cx) = cx.add_window_view(|_, _| SelectionView {
            text_state: text_state.clone(),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        let (start, end, origin_scope, neighbor_scope) = cx.update(|_, app| {
            let state = text_state.read(app);
            let origin = state
                .selectable_text
                .iter()
                .find(|text| text.text.contains("Alpha cell content"))
                .expect("origin table cell should be registered");
            let neighbor = state
                .selectable_text
                .iter()
                .find(|text| text.text.contains("Beta must be selectable"))
                .expect("neighbor table cell should be registered");
            (
                origin
                    .layout
                    .position_for_index(0)
                    .expect("origin text should have a rendered position")
                    + point(px(1.0), origin.layout.line_height().half()),
                neighbor
                    .layout
                    .position_for_index(neighbor.text.len())
                    .expect("neighbor text should have a rendered end position")
                    + point(px(1.0), neighbor.layout.line_height().half()),
                origin.selection_scope,
                neighbor.selection_scope,
            )
        });
        assert!(origin_scope.is_some());
        assert_ne!(origin_scope, neighbor_scope);

        cx.update(|window, app| {
            text_state.update(app, |state, _| {
                state.start_selection(start);
                state.update_selection(end);
                state.end_selection();
            });
            window.refresh();
        });
        cx.run_until_parked();
        cx.update(|_, app| {
            let selected = text_state
                .read(app)
                .selection_text()
                .expect("the table cells should have selected text");
            assert!(selected.contains("Alpha cell content"));
            assert!(selected.contains("Beta must be selectable"));
            assert!(!selected.contains("Score"));
        });
    }

    #[test]
    fn streaming_updates_reuse_only_unchanged_block_heights() {
        let parse = |text| {
            parse_content(
                TextViewType::Markdown,
                text,
                TextViewStyle::default(),
                HighlightTheme::default_light().as_ref(),
                &None,
            )
        };
        let original = parse("# Heading\n\nParagraph\n\n```rust\nlet value = 1;\n```\n");
        let appended =
            parse("# Heading\n\nParagraph\n\n```rust\nlet value = 1;\n```\n\nAnother paragraph\n");
        let changed_tail = parse("# Heading\n\nParagraph\n\n```rust\nlet value = 2;\n```\n");

        assert_eq!(matching_root_block_prefix(Some(&original), &appended), 3);
        assert_eq!(
            matching_root_block_prefix(Some(&original), &changed_tail),
            2
        );
    }

    #[gpui::test]
    fn block_height_changes_exclude_initial_measurement_and_subpixel_noise(
        cx: &mut gpui::TestAppContext,
    ) {
        let state = cx.new(TextViewState::new);
        assert_eq!(
            state.update(cx, |state, _| {
                state.record_block_height(2, px(100.0), px(90.0))
            }),
            None
        );
        assert_eq!(
            state.update(cx, |state, _| {
                state.record_block_height(2, px(100.25), px(90.0))
            }),
            None
        );
        assert_eq!(
            state.update(cx, |state, _| {
                state.record_block_height(2, px(148.0), px(90.0))
            }),
            Some((px(47.75), false))
        );
    }

    #[gpui::test]
    fn width_changes_rebase_existing_runtime_height_corrections(cx: &mut gpui::TestAppContext) {
        let state = cx.new(TextViewState::new);
        state.update(cx, |state, _| {
            state.block_resize_handler = Some(Arc::new(|_, _| {}));
            assert_eq!(state.record_block_height(0, px(100.0), px(90.0)), None);
            assert_eq!(
                state.record_block_height(0, px(148.0), px(90.0)),
                Some((px(48.0), false))
            );

            state.invalidate_block_heights_for_width();
            assert_eq!(
                state.record_block_height(0, px(180.0), px(120.0)),
                Some((px(12.0), true))
            );
            assert_eq!(state.block_reported_deltas, vec![px(60.0)]);
        });
    }

    #[gpui::test]
    fn remount_reset_discards_the_previous_parent_scroll_geometry(
        cx: &mut gpui::TestAppContext,
    ) {
        let text_state = cx.new(TextViewState::new);
        let viewport = TextViewScrollViewport {
            bounds: Bounds::new(point(px(0.0), px(40.0)), size(px(700.0), px(600.0))),
            scroll_offset: Pixels::ZERO,
        };
        text_state.update(cx, |state, _| {
            state.bounds = Bounds::new(point(px(0.0), px(120.0)), size(px(700.0), px(900.0)));
            state.parent_scroll_offset = px(-360.0);
        });

        let stale_origin = text_state.update(cx, |state, _| {
            state
                .block_virtualization(viewport, 4, text_state.clone(), None)
                .expect("expected block virtualization")
                .content_origin_y
        });
        assert_eq!(stale_origin, px(480.0));

        text_state.update(cx, |state, _| state.reset_block_viewport_layout());
        let remounted_origin = text_state.update(cx, |state, _| {
            state
                .block_virtualization(viewport, 4, text_state.clone(), None)
                .expect("expected block virtualization")
                .content_origin_y
        });
        assert_eq!(remounted_origin, viewport.bounds.origin.y);
    }

    #[gpui::test]
    fn reparsing_removes_resize_adjustments_from_the_changed_tail(cx: &mut gpui::TestAppContext) {
        let parse = |text| {
            parse_content(
                TextViewType::Markdown,
                text,
                TextViewStyle::default(),
                HighlightTheme::default_light().as_ref(),
                &None,
            )
        };
        let state = cx.new(TextViewState::new);
        state.update(cx, |state, _| {
            assert!(
                state
                    .replace_parsed_result(parse("First\n\nSecond"))
                    .is_none()
            );
            state.block_reported_deltas = vec![px(12.0), px(48.0)];
        });

        let removed = state.update(cx, |state, _| {
            state.replace_parsed_result(parse("First\n\nChanged"))
        });
        assert_eq!(
            removed,
            Some(TextViewBlockResize {
                block_index: 1,
                delta: px(-48.0),
                above_viewport: false,
            })
        );
        assert_eq!(
            cx.update(|app| state.read(app).block_reported_deltas.clone()),
            vec![px(12.0), px(0.0)]
        );
    }

    #[gpui::test]
    fn streaming_reparse_keeps_a_disclosure_open(cx: &mut gpui::TestAppContext) {
        let parse = |body| {
            parse_content(
                TextViewType::Markdown,
                &format!("<details><summary>Diagnostics</summary>\n\n{body}\n</details>"),
                TextViewStyle::default(),
                HighlightTheme::default_light().as_ref(),
                &None,
            )
        };
        let state = cx.new(TextViewState::new);
        state.update(cx, |state, _| {
            state.replace_parsed_result(parse("First line"));
            let details = state
                .parsed_result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(|content| content.root_node.root_blocks().first())
                .and_then(|node| match node {
                    node::Node::Details(details) => Some(details),
                    _ => None,
                })
                .expect("expected parsed details");
            details.set_expanded(true);

            state.replace_parsed_result(parse("First line\n\nStreamed line"));
        });

        let expanded = state.read_with(cx, |state, _| {
            state
                .parsed_result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(|content| content.root_node.root_blocks().first())
                .is_some_and(
                    |node| matches!(node, node::Node::Details(details) if details.is_expanded()),
                )
        });
        assert!(
            expanded,
            "streaming body updates must retain disclosure state"
        );
    }

    #[gpui::test]
    fn details_expand_and_collapse_emit_runtime_height_changes(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let text_state = cx.new(TextViewState::new);
        let scroll_state = ListState::new(1, ListAlignment::Top, px(512.0));
        let resize_events = Arc::new(std::sync::Mutex::new(Vec::<TextViewBlockResize>::new()));
        let markdown: SharedString = indoc::indoc! {r#"
            <details>
            <summary>Diagnostics</summary>

            First paragraph with enough text to occupy a line.

            Second paragraph with enough text to occupy another line.
            </details>
        "#}
        .into();

        struct DisclosureView {
            text_state: Entity<TextViewState>,
            scroll_state: ListState,
            resize_events: Arc<std::sync::Mutex<Vec<TextViewBlockResize>>>,
            markdown: SharedString,
        }

        impl gpui::Render for DisclosureView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let viewport = TextViewScrollViewport::from_list(&self.scroll_state);
                let text_state = self.text_state.clone();
                let markdown = self.markdown.clone();
                let events = self.resize_events.clone();
                gpui::list(self.scroll_state.clone(), move |_, _, cx| {
                    let events = events.clone();
                    TextView::markdown_with_state(
                        "dynamic-details",
                        markdown.clone(),
                        text_state.clone(),
                        cx,
                    )
                    .block_viewport(viewport)
                    .block_layout_width(px(700.0))
                    .selectable(true)
                    .on_block_resize(move |event, _| {
                        events.lock().unwrap().push(event);
                    })
                    .w_full()
                    .into_any_element()
                })
                .w(px(700.0))
                .h(px(500.0))
            }
        }

        let (_view, cx) = cx.add_window_view(|_, _| DisclosureView {
            text_state: text_state.clone(),
            scroll_state,
            resize_events: resize_events.clone(),
            markdown,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(resize_events.lock().unwrap().is_empty());
        let summary = cx
            .debug_bounds("markdown-details-summary")
            .expect("details summary should be rendered");
        cx.simulate_mouse_move(summary.center(), None, Modifiers::default());
        cx.simulate_click(summary.center(), Modifiers::default());
        let expanded = cx.update(|_, app| {
            text_state
                .read(app)
                .parsed_result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(|content| content.root_node.root_blocks().first())
                .is_some_and(
                    |node| matches!(node, node::Node::Details(details) if details.is_expanded()),
                )
        });
        assert!(expanded, "click should open the parsed details node");
        let is_selecting = cx.update(|_, app| text_state.read(app).is_selecting);
        assert!(
            !is_selecting,
            "clicking a disclosure must not leave text selection active"
        );
        cx.run_until_parked();
        let events_after_expand = resize_events.lock().unwrap().clone();
        assert!(
            events_after_expand
                .last()
                .is_some_and(|event| event.delta > Pixels::ZERO),
            "expected a positive resize after expanding details, got {events_after_expand:?}"
        );

        let summary = cx
            .debug_bounds("markdown-details-summary")
            .expect("details summary should remain rendered");
        cx.simulate_mouse_move(summary.center(), None, Modifiers::default());
        cx.simulate_click(summary.center(), Modifiers::default());
        cx.run_until_parked();
        let events_after_collapse = resize_events.lock().unwrap().clone();
        assert!(
            events_after_collapse
                .last()
                .is_some_and(|event| event.delta < Pixels::ZERO),
            "expected a negative resize after collapsing details, got {events_after_collapse:?}"
        );
    }

    #[gpui::test]
    fn mixed_markdown_renders_a_bounded_block_window(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let text_state = cx.new(TextViewState::new);
        let scroll_state = ListState::new(1, ListAlignment::Top, px(512.0));
        let mut markdown = String::new();
        for index in 0..80 {
            writeln!(markdown, "# Heading {index}\n").unwrap();
            writeln!(
                markdown,
                "Paragraph {index} contains **bold**, _italic_, and [linked](https://example.com) text.\n"
            )
            .unwrap();
            writeln!(
                markdown,
                "> Quoted block {index} with a little more text.\n"
            )
            .unwrap();
            writeln!(markdown, "- item {index}.1\n- item {index}.2\n").unwrap();
            writeln!(
                markdown,
                "| Name | Value |\n| --- | --- |\n| row {index} | value {index} |\n"
            )
            .unwrap();
            writeln!(markdown, "```\nlet value_{index} = {index};\n```\n").unwrap();
        }
        let markdown: SharedString = markdown.into();

        struct StressView {
            text_state: Entity<TextViewState>,
            scroll_state: ListState,
            markdown: SharedString,
        }

        impl gpui::Render for StressView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let block_viewport = TextViewScrollViewport::from_list(&self.scroll_state);
                let text_state = self.text_state.clone();
                let markdown = self.markdown.clone();
                let text_scroll_state = self.scroll_state.clone();
                gpui::list(self.scroll_state.clone(), move |_, _, cx| {
                    TextView::markdown_with_state(
                        "mixed-markdown-stress",
                        markdown.clone(),
                        text_state.clone(),
                        cx,
                    )
                    .selectable(true)
                    .selection_scroll_handle(&text_scroll_state)
                    .block_viewport(block_viewport)
                    .w_full()
                    .into_any_element()
                })
                .size_full()
            }
        }

        let view = cx.new(|_| StressView {
            text_state: text_state.clone(),
            scroll_state: scroll_state.clone(),
            markdown,
        });

        let cx = cx.add_empty_window();
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.draw(
                point(px(0.0), px(0.0)),
                size(px(900.0), px(700.0)),
                |_, _| view.clone(),
            );
        };

        draw(cx);
        let total_blocks = cx.update(|_, app| {
            text_state
                .read(app)
                .parsed_result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map_or(0, |content| content.root_node.root_block_count())
        });
        assert!(total_blocks >= 400);
        let first_frame_blocks = cx.update(|_, app| text_state.read(app).rendered_block_count());
        assert!(first_frame_blocks > 0);
        assert!(first_frame_blocks < total_blocks / 4);

        draw(cx);
        let rendered_blocks = cx.update(|_, app| text_state.read(app).rendered_block_count());
        assert!(rendered_blocks > 0);
        assert!(rendered_blocks < total_blocks / 4);

        scroll_state.scroll_by(px(5_000.0));
        draw(cx);
        let rendered_after_scroll = cx.update(|_, app| text_state.read(app).rendered_block_count());
        assert!(rendered_after_scroll > 0);
        assert!(rendered_after_scroll < total_blocks / 4);

        cx.update(|_, app| {
            text_state.update(app, |state, _| {
                state.start_selection(state.bounds.origin + point(px(20.0), px(20.0)));
            });
        });
        draw(cx);
        assert_eq!(
            cx.update(|_, app| text_state.read(app).rendered_block_count()),
            total_blocks
        );

        cx.update(|_, app| {
            text_state.update(app, |state, _| state.end_selection());
        });
        draw(cx);
        draw(cx);
        assert!(cx.update(|_, app| text_state.read(app).rendered_block_count()) < total_blocks / 4);
    }
}
