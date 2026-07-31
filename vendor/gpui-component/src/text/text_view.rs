use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    Entity, EntityId, FocusHandle, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId,
    InteractiveElement, IntoElement, KeyBinding, LayoutId, ListState, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, RenderOnce, SharedString, Size,
    StyleRefinement, Styled, Timer, Window, div, px,
};
use smol::stream::StreamExt;

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
    state: Entity<TextViewState>,
}

impl RenderOnce for TextViewElement {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| {
            v_flex()
                .size_full()
                .map(|this| match &mut state.parsed_result {
                    Some(Ok(content)) => this.child(content.root_node.render_root(
                        self.list_state.clone(),
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
    update_delay: Duration,
    code_block_actions: Option<Arc<CodeBlockActionsFn>>,
}

#[derive(PartialEq)]
pub(crate) struct ParsedContent {
    pub(crate) root_node: node::Node,
    pub(crate) node_cx: node::NodeContext,
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
    /// Is current in selection.
    is_selecting: bool,
    is_selectable: bool,
    list_state: ListState,
    current_text: SharedString,
    current_style: TextViewStyle,
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
            is_selecting: false,
            is_selectable: false,
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(1000.)),
            current_text: SharedString::default(),
            current_style: TextViewStyle::default(),
        }
    }
}

impl TextViewState {
    /// Save bounds and unselect only when width changes can reflow text.
    /// Height-only changes are normal while a variable-height parent list
    /// settles its measurements and do not invalidate document coordinates.
    fn update_bounds(&mut self, bounds: Bounds<Pixels>) {
        if self.bounds.size.width > Pixels::ZERO
            && bounds.size.width > Pixels::ZERO
            && self.bounds.size.width != bounds.size.width
            && self.selection_pointer.is_none()
        {
            self.clear_selection();
        }
        self.bounds = bounds;
    }

    fn clear_selection(&mut self) {
        self.selection_positions = (None, None);
        self.selection_pointer = None;
        self.selection_autoscroll_active = false;
        self.is_selecting = false;
    }

    fn start_selection(&mut self, pos: Point<Pixels>) {
        let pos = pos - self.bounds.origin;
        self.selection_positions = (Some(pos), Some(pos));
        self.selection_pointer = Some(pos + self.bounds.origin);
        self.selection_autoscroll_active = false;
        self.is_selecting = true;
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

            self.state.update(cx, {
                let tx = tx.clone();
                |state, _| {
                    state.parsed_result = Some(parsed_result);
                    state.tx = Some(tx);
                    state.current_text = text.clone();
                    state.current_style = style.clone();
                }
            });

            cx.spawn({
                let state = self.state.downgrade();
                async move |cx| {
                    while let Ok(parsed_result) = rx_result.recv().await {
                        if let Some(state) = state.upgrade() {
                            _ = state.update(cx, |state, cx| {
                                state.parsed_result = Some(parsed_result);
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
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
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

        if is_selectable {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }

        self.state.update(cx, |state, _| {
            state.parent_entity = Some(entity_id);
            state.update_bounds(bounds);
            if state.selection_pointer.is_some() {
                state.update_selection_from_pointer();
                if !state.is_selecting {
                    // Mouse-up may arrive before the list paints its final
                    // autoscroll offset. Retain the pointer until this paint,
                    // then keep only the settled document-local endpoint.
                    state.selection_pointer = None;
                }
            }
            state.is_selectable = is_selectable;
        });

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
                move |event: &MouseDownEvent, phase, _, cx| {
                    if !bounds.contains(&event.position) || !phase.bubble() {
                        return;
                    }

                    state.update(cx, |state, _| {
                        state.start_selection(event.position);
                    });
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
                move |_: &MouseUpEvent, _, _, cx| {
                    if !state.read(cx).is_selecting {
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
                        if bounds.contains(&event.position) {
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
    use gpui::{Bounds, point, px, size};

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
}
