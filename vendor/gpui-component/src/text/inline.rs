use std::{
    ops::Range,
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui::{
    App, BorderStyle, Bounds, CursorStyle, Edges, Element, ElementId, GlobalElementId, Half,
    HighlightStyle, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, StyledText, TextLayout, Window,
    point, px, quad,
};

use crate::{ActiveTheme, global_state::GlobalState, input::Selection, text::node::LinkMark};

// GPUI's text runs have no inline box model. Non-breaking spaces provide real
// shaped layout gutters, while the adjacent word joiners keep those gutters
// attached to the code span during wrapping. Copying strips both gutters.
const LEADING_CODE_GUTTER: &str = "\u{00a0}\u{2060}";
const TRAILING_CODE_GUTTER: &str = "\u{2060}\u{00a0}";

struct LayoutText {
    text: SharedString,
    code_ranges: Vec<Range<usize>>,
    virtual_gutters: Vec<Range<usize>>,
    insertions: Vec<LayoutInsertion>,
}

struct LayoutInsertion {
    source_offset: usize,
    byte_len: usize,
    before_source_boundary: bool,
}

#[derive(Debug)]
struct CodeBackgroundSegment {
    left: Pixels,
    right: Pixels,
    top: Pixels,
}

fn append_code_background_segment(
    segments: &mut Vec<CodeBackgroundSegment>,
    bounds: &Bounds<Pixels>,
    start: Point<Pixels>,
    end: Point<Pixels>,
) {
    // GPUI maps an index exactly on a soft-wrap boundary to the end of the
    // preceding visual line. If the next character position is on a later
    // line, that character belongs to the later line; painting from `start`
    // to the old line edge would leave a phantom capsule behind.
    let (left, right, top) = if start.y == end.y {
        (start.x.min(end.x), start.x.max(end.x), start.y)
    } else {
        (bounds.left(), end.x, end.y)
    };
    let left = left.max(bounds.left());
    let right = right.min(bounds.right());
    if right <= left {
        return;
    }

    if let Some(segment) = segments.last_mut().filter(|segment| segment.top == top) {
        segment.left = segment.left.min(left);
        segment.right = segment.right.max(right);
    } else {
        segments.push(CodeBackgroundSegment { left, right, top });
    }
}

impl LayoutText {
    fn new(source: &str, code_ranges: &[Range<usize>]) -> Self {
        let mut source_ranges = code_ranges
            .iter()
            .filter(|range| {
                range.start < range.end
                    && range.end <= source.len()
                    && source.is_char_boundary(range.start)
                    && source.is_char_boundary(range.end)
            })
            .cloned()
            .collect::<Vec<_>>();
        source_ranges.sort_by_key(|range| (range.start, range.end));

        let mut text = String::with_capacity(
            source.len()
                + source_ranges.len() * (LEADING_CODE_GUTTER.len() + TRAILING_CODE_GUTTER.len()),
        );
        let mut layout_code_ranges = Vec::with_capacity(source_ranges.len());
        let mut virtual_gutters = Vec::with_capacity(source_ranges.len() * 2);
        let mut insertions = Vec::with_capacity(source_ranges.len() * 2);
        let mut source_cursor = 0;

        for range in source_ranges {
            if range.start < source_cursor {
                continue;
            }

            text.push_str(&source[source_cursor..range.start]);

            let capsule_start = text.len();
            text.push_str(LEADING_CODE_GUTTER);
            virtual_gutters.push(capsule_start..text.len());
            insertions.push(LayoutInsertion {
                source_offset: range.start,
                byte_len: LEADING_CODE_GUTTER.len(),
                before_source_boundary: false,
            });

            text.push_str(&source[range.clone()]);

            let trailing_start = text.len();
            text.push_str(TRAILING_CODE_GUTTER);
            virtual_gutters.push(trailing_start..text.len());
            insertions.push(LayoutInsertion {
                source_offset: range.end,
                byte_len: TRAILING_CODE_GUTTER.len(),
                before_source_boundary: true,
            });
            layout_code_ranges.push(capsule_start..text.len());
            source_cursor = range.end;
        }

        text.push_str(&source[source_cursor..]);

        Self {
            text: text.into(),
            code_ranges: layout_code_ranges,
            virtual_gutters,
            insertions,
        }
    }

    fn map_source_range(&self, range: Range<usize>) -> Range<usize> {
        self.map_source_boundary(range.start)..self.map_source_boundary(range.end)
    }

    fn map_source_boundary(&self, source_offset: usize) -> usize {
        source_offset
            + self
                .insertions
                .iter()
                .filter(|insertion| {
                    insertion.source_offset < source_offset
                        || (insertion.source_offset == source_offset
                            && insertion.before_source_boundary)
                })
                .map(|insertion| insertion.byte_len)
                .sum::<usize>()
    }
}

/// A inline element used to render a inline text and support selectable.
///
/// All text in TextView (including the CodeBlock) used this for text rendering.
pub(super) struct Inline {
    id: ElementId,
    text: SharedString,
    selection_scope: Option<usize>,
    links: Rc<Vec<(Range<usize>, LinkMark)>>,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    code_ranges: Vec<Range<usize>>,
    styled_text: StyledText,

    state: Arc<Mutex<InlineState>>,
}

/// The inline text state, used RefCell to keep the selection state.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct InlineState {
    hovered_index: Option<usize>,
    /// The text that actually rendering, matched with selection.
    pub(super) text: SharedString,
    virtual_gutters: Vec<Range<usize>>,
    pub(super) selection: Option<Selection>,
}

impl InlineState {
    /// Save actually rendered text for selected text to use.
    pub(crate) fn set_text(&mut self, text: SharedString) {
        self.text = text;
        self.virtual_gutters.clear();
    }

    fn set_layout_text(&mut self, text: SharedString, virtual_gutters: Vec<Range<usize>>) {
        self.text = text;
        self.virtual_gutters = virtual_gutters;
    }

    pub(super) fn selected_text(&self) -> String {
        let Some(selection) = &self.selection else {
            return String::new();
        };
        let start = selection.start.min(selection.end).min(self.text.len());
        let end = selection.start.max(selection.end).min(self.text.len());
        if start >= end {
            return String::new();
        }

        let mut selected = String::new();
        let mut cursor = start;
        for gutter in &self.virtual_gutters {
            if gutter.end <= cursor {
                continue;
            }
            if gutter.start >= end {
                break;
            }
            if cursor < gutter.start {
                selected.push_str(&self.text[cursor..gutter.start.min(end)]);
            }
            cursor = cursor.max(gutter.end.min(end));
        }
        if cursor < end {
            selected.push_str(&self.text[cursor..end]);
        }
        selected
    }
}

impl Inline {
    pub(super) fn new(
        id: impl Into<ElementId>,
        state: Arc<Mutex<InlineState>>,
        links: Vec<(Range<usize>, LinkMark)>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        code_ranges: Vec<Range<usize>>,
    ) -> Self {
        let source_text = state.lock().unwrap().text.clone();
        let layout_text = LayoutText::new(&source_text, &code_ranges);
        let links = links
            .into_iter()
            .map(|(range, mark)| (layout_text.map_source_range(range), mark))
            .collect();
        let highlights = highlights
            .into_iter()
            .map(|(range, style)| (layout_text.map_source_range(range), style))
            .collect();
        let text = layout_text.text.clone();
        state
            .lock()
            .unwrap()
            .set_layout_text(text.clone(), layout_text.virtual_gutters);
        Self {
            id: id.into(),
            links: Rc::new(links),
            highlights,
            code_ranges: layout_text.code_ranges,
            text: text.clone(),
            selection_scope: None,
            styled_text: StyledText::new(text),
            state,
        }
    }

    pub(super) fn selection_scope(mut self, selection_scope: Option<usize>) -> Self {
        self.selection_scope = selection_scope;
        self
    }

    /// Get link at given mouse position.
    fn link_for_position(
        layout: &TextLayout,
        links: &Vec<(Range<usize>, LinkMark)>,
        position: Point<Pixels>,
    ) -> Option<LinkMark> {
        let offset = layout.index_for_position(position).ok()?;
        for (range, link) in links.iter() {
            if range.contains(&offset) {
                return Some(link.clone());
            }
        }

        None
    }

    /// Paint selected bounds for debug.
    #[allow(unused)]
    fn paint_selected_bounds(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.paint_quad(gpui::PaintQuad {
            bounds,
            background: cx.theme().blue.alpha(0.01).into(),
            corner_radii: gpui::Corners::default(),
            border_color: gpui::transparent_black(),
            border_style: BorderStyle::default(),
            border_widths: gpui::Edges::all(px(0.)),
        });
    }

    fn layout_selections(
        &self,
        text_layout: &TextLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> (bool, bool, Option<Selection>) {
        let Some(text_view_state) = GlobalState::global(cx).text_view_state() else {
            return (false, false, None);
        };

        let text_view_state = text_view_state.read(cx);
        let is_selectable = text_view_state.is_selectable();
        if !text_view_state.has_selection() {
            return (is_selectable, false, None);
        }
        if !text_view_state.selection_allows_scope(self.selection_scope) {
            return (is_selectable, false, None);
        }

        let line_height = window.line_height();
        let selection_bounds = text_view_state.selection_bounds();

        // Use for debug selection bounds
        // self.paint_selected_bounds(selection_bounds, window, cx);

        let mut selection: Option<Selection> = None;
        let mut offset = 0;
        let mut chars = self.text.chars().peekable();
        while let Some(c) = chars.next() {
            let Some(pos) = text_layout.position_for_index(offset) else {
                offset += c.len_utf8();
                continue;
            };

            let mut char_width = line_height.half();
            if let Some(next_pos) = text_layout.position_for_index(offset + 1) {
                if next_pos.y == pos.y {
                    char_width = next_pos.x - pos.x;
                }
            }

            if point_in_text_selection(pos, char_width, &selection_bounds, line_height) {
                if selection.is_none() {
                    selection = Some((offset..offset).into());
                }

                let next_offset = offset + c.len_utf8();
                selection.as_mut().unwrap().end = next_offset;
            }

            offset += c.len_utf8();
        }

        (true, true, selection)
    }

    /// Paint the selection background.
    fn paint_selection(
        selection: &Selection,
        text_layout: &TextLayout,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut start = selection.start;
        let mut end = selection.end;
        if end < start {
            std::mem::swap(&mut start, &mut end);
        }
        let Some(start_position) = text_layout.position_for_index(start) else {
            return;
        };
        let Some(end_position) = text_layout.position_for_index(end) else {
            return;
        };

        let line_height = text_layout.line_height();
        if start_position.y == end_position.y {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        } else {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(bounds.right(), start_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));

            if end_position.y > start_position.y + line_height {
                window.paint_quad(quad(
                    Bounds::from_corners(
                        point(bounds.left(), start_position.y + line_height),
                        point(bounds.right(), end_position.y),
                    ),
                    px(0.),
                    cx.theme().selection,
                    Edges::default(),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
            }

            window.paint_quad(quad(
                Bounds::from_corners(
                    point(bounds.left(), end_position.y),
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }

    /// Paint inline code as compact capsules instead of using a text-run
    /// background, which fills the entire line height and visually joins code
    /// on adjacent lines.
    fn paint_code_backgrounds(
        &self,
        text_layout: &TextLayout,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line_height = text_layout.line_height();
        let vertical_inset = px(2.0);
        let corner_radius = px(4.0);
        let background = if cx.theme().is_dark() {
            gpui::hsla(209.26 / 360.0, 0.010_193, 0.149_55, 1.0)
        } else {
            gpui::hsla(307.61 / 360.0, 0.019_79, 0.896_49, 1.0)
        };

        for range in &self.code_ranges {
            let Some(code_text) = self.text.get(range.clone()) else {
                continue;
            };
            let mut segments = Vec::new();
            let mut index = range.start;
            for character in code_text.chars() {
                let next_index = index + character.len_utf8();
                if let (Some(start), Some(end)) = (
                    text_layout.position_for_index(index),
                    text_layout.position_for_index(next_index),
                ) {
                    append_code_background_segment(&mut segments, bounds, start, end);
                }
                index = next_index;
            }

            for segment in segments {
                window.paint_quad(quad(
                    Bounds::from_corners(
                        point(segment.left, segment.top + vertical_inset),
                        point(segment.right, segment.top + line_height - vertical_inset),
                    ),
                    corner_radius,
                    background,
                    Edges::default(),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
            }
        }
    }
}

impl IntoElement for Inline {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Inline {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_element_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();

        let mut runs = Vec::new();
        let mut ix = 0;
        for (range, highlight) in self.highlights.iter() {
            if ix < range.start {
                runs.push(text_style.clone().to_run(range.start - ix));
            }
            runs.push(text_style.clone().highlight(*highlight).to_run(range.len()));
            ix = range.end;
        }
        if ix < self.text.len() {
            runs.push(text_style.to_run(self.text.len() - ix));
        }

        self.styled_text = StyledText::new(self.text.clone()).with_runs(runs);
        let (layout_id, _) =
            self.styled_text
                .request_layout(global_element_id, inspector_id, window, cx);

        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled_text
            .prepaint(id, inspector_id, bounds, &mut (), window, cx);

        (!self.links.is_empty()).then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut state = self.state.lock().unwrap();

        let text_layout = self.styled_text.layout().clone();
        // layout selections
        let (_, is_selection, selection) = self.layout_selections(&text_layout, window, cx);

        state.selection = selection;

        self.paint_code_backgrounds(&text_layout, &bounds, window, cx);

        if let Some(selection) = &state.selection {
            Self::paint_selection(selection, &text_layout, &bounds, window, cx);
        }

        // Paint glyphs after the selection fill so an obvious selection color
        // does not wash out the selected text.
        self.styled_text
            .paint(global_id, None, bounds, &mut (), &mut (), window, cx);

        if let Some(text_view_state) = GlobalState::global(cx).text_view_state().cloned() {
            text_view_state.update(cx, |state, _| {
                state.record_selectable_text(
                    bounds,
                    self.text.clone(),
                    text_layout.clone(),
                    self.selection_scope,
                );
            });
        }

        if let Some(hitbox) = prepaint {
            let current_view = window.current_view();
            let mouse_position = window.mouse_position();
            if Self::link_for_position(&text_layout, &self.links, mouse_position).is_some() {
                window.set_cursor_style(CursorStyle::PointingHand, hitbox);
            }

            window.on_mouse_event({
                let hitbox = hitbox.clone();
                let text_layout = text_layout.clone();
                let mut hovered_index = state.hovered_index;
                move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.bubble() || !hitbox.is_hovered(window) {
                        return;
                    }

                    let current = hovered_index;
                    let updated = text_layout.index_for_position(event.position).ok();
                    if current != updated {
                        hovered_index = updated;
                        cx.notify(current_view);
                    }
                }
            });

            if !is_selection {
                window.on_mouse_event({
                    let links = self.links.clone();
                    let text_layout = text_layout.clone();

                    move |event: &MouseUpEvent, phase, _, cx| {
                        if !bounds.contains(&event.position) || !phase.bubble() {
                            return;
                        }

                        if let Some(link) =
                            Self::link_for_position(&text_layout, &links, event.position)
                        {
                            cx.stop_propagation();
                            cx.open_url(&link.url);
                        }
                    }
                });
            }
        }
    }
}

/// Check if a `pos` is within a `bounds`, considering multi-line selections.
fn point_in_text_selection(
    pos: Point<Pixels>,
    char_width: Pixels,
    bounds: &Bounds<Pixels>,
    line_height: Pixels,
) -> bool {
    let top = bounds.top();
    let bottom = bounds.bottom();
    let left = bounds.left();
    let right = bounds.right();

    // Out of the vertical bounds
    if pos.y + line_height < top || pos.y >= bottom {
        return false;
    }

    let single_line = (bottom - top) <= line_height;
    if single_line {
        // If it's a single line selection, just check horizontal bounds
        return pos.x + char_width.half() >= left && pos.x + char_width.half() <= right;
    }

    let is_above = pos.y <= top;
    let is_below = pos.y + line_height >= bottom;

    if is_above {
        return pos.x + char_width.half() >= left;
    } else if is_below {
        return pos.x + char_width.half() <= right;
    } else {
        return true;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InlineState, LEADING_CODE_GUTTER, LayoutText, TRAILING_CODE_GUTTER,
        append_code_background_segment, point_in_text_selection,
    };
    use gpui::{Bounds, point, px, size};

    #[test]
    fn inline_code_gutters_reserve_space_without_changing_selected_text() {
        let source = "Run cargo now";
        let layout = LayoutText::new(source, &[4..9]);
        let expected = format!("Run {LEADING_CODE_GUTTER}cargo{TRAILING_CODE_GUTTER} now");
        let capsule_end =
            4 + LEADING_CODE_GUTTER.len() + "cargo".len() + TRAILING_CODE_GUTTER.len();

        assert_eq!(layout.text.as_ref(), expected.as_str());
        assert_eq!(layout.code_ranges, vec![4..capsule_end]);
        assert_eq!(layout.map_source_range(0..4), 0..4);
        assert_eq!(layout.map_source_range(4..9), layout.code_ranges[0]);
        assert_eq!(
            layout.map_source_range(9..source.len()),
            capsule_end..layout.text.len()
        );

        let code_range = layout.code_ranges[0].clone();
        let mut state = InlineState::default();
        state.set_layout_text(layout.text, layout.virtual_gutters);
        state.selection = Some((0..state.text.len()).into());
        assert_eq!(state.selected_text(), source);

        state.selection = Some(code_range.into());
        assert_eq!(state.selected_text(), "cargo");
    }

    #[test]
    fn inline_code_background_starts_on_the_line_containing_the_wrapped_character() {
        let bounds = Bounds::from_corners(point(px(10.), px(0.)), point(px(110.), px(40.)));
        let mut segments = Vec::new();

        // The first code character starts at a soft-wrap boundary. GPUI
        // reports that boundary at the end of the old line and its trailing
        // edge on the new line.
        append_code_background_segment(
            &mut segments,
            &bounds,
            point(px(80.), px(0.)),
            point(px(20.), px(20.)),
        );
        append_code_background_segment(
            &mut segments,
            &bounds,
            point(px(20.), px(20.)),
            point(px(50.), px(20.)),
        );

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].left, px(10.));
        assert_eq!(segments[0].right, px(50.));
        assert_eq!(segments[0].top, px(20.));
    }

    #[test]
    fn test_point_in_text_selection() {
        let line_height = px(20.);
        let char_width = px(10.);
        let bounds = Bounds {
            origin: point(px(50.), px(50.)),
            size: size(px(100.), px(100.)),
        };

        // First line but haft line height, true
        // | p --------|
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(50.), px(40.)),
            char_width,
            &bounds,
            line_height
        ));

        // First line in selection, true
        // | p --------|
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(50.), px(50.)),
            char_width,
            &bounds,
            line_height
        ));
        // First line, but left out of selection, false
        // p |-----------|
        //   | selection |
        //   |-----------|
        assert!(!point_in_text_selection(
            point(px(40.), px(50.)),
            char_width,
            &bounds,
            line_height
        ));
        // First line but right out of selection, true
        // |-----------| p
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(160.), px(50.)),
            char_width,
            &bounds,
            line_height
        ));

        // Middle line in selection, true
        // |-----------|
        // |     p     |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(100.), px(70.)),
            char_width,
            &bounds,
            line_height
        ));
        // Middle line, but left out of selection, true
        //   |-----------|
        // p | selection |
        //   |-----------|
        assert!(point_in_text_selection(
            point(px(40.), px(70.)),
            char_width,
            &bounds,
            line_height
        ));
        // Middle line, but right out of selection, true
        // |-----------|
        // | selection | p
        // |-----------|
        assert!(point_in_text_selection(
            point(px(160.), px(70.)),
            char_width,
            &bounds,
            line_height
        ));

        // Last line in selection, true
        // |-----------|
        // | selection |
        // |------- p -|
        assert!(point_in_text_selection(
            point(px(100.), px(140.)),
            char_width,
            &bounds,
            line_height
        ));
        // Last line, but left out of selection, true
        //
        //   |-----------|
        //   | selection |
        // p |-----------|
        assert!(point_in_text_selection(
            point(px(40.), px(140.)),
            char_width,
            &bounds,
            line_height
        ));
        // Last line, but right out of selection, false
        // |-----------|
        // | selection |
        // |-----------| p
        assert!(!point_in_text_selection(
            point(px(160.), px(140.)),
            char_width,
            &bounds,
            line_height
        ));

        // Out of vertical bounds (top), false
        //       p
        // |-----------|
        // | selection |
        // |-----------|
        assert!(!point_in_text_selection(
            point(px(100.), px(20.)),
            char_width,
            &bounds,
            line_height
        ));
        // Out of vertical bounds (bottom), false
        // |-----------|
        // | selection |
        // |-----------|
        //       p
        assert!(!point_in_text_selection(
            point(px(100.), px(160.)),
            char_width,
            &bounds,
            line_height
        ));
    }
}
