use std::ops::Range;

use gpui::{
    App, Div, ElementId, Hsla, InteractiveElement, Interactivity, RenderOnce, SharedString,
    Stateful, StyleRefinement, Styled, Svg, Window, div, prelude::*, px, rgb, svg,
};
use gpui_component::{Selectable, menu::DropdownMenu};

use crate::model::{ActivityKind, ProviderKind, SessionStatus, unix_time};
use crate::theme::Theme;

/// A monochrome icon from the embedded set, tinted via text color.
pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(color)
}

/// Muted brand hue for each provider's mark.
pub fn provider_color(provider: ProviderKind) -> Hsla {
    match provider {
        ProviderKind::Claude => rgb(0xCC7B5E).into(),
        ProviderKind::Codex => rgb(0xA9B1BC).into(),
        ProviderKind::OpenCode => rgb(0x94BBA4).into(),
        ProviderKind::Grok => rgb(0xB3B9C3).into(),
    }
}

/// Simple geometric mark per provider, drawn from the embedded icon set.
pub fn provider_icon(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Claude => "icons/sparkle.svg",
        ProviderKind::Codex => "icons/hexagon.svg",
        ProviderKind::OpenCode => "icons/block.svg",
        ProviderKind::Grok => "icons/slash.svg",
    }
}

pub fn status_color(theme: &Theme, status: SessionStatus) -> Hsla {
    match status {
        SessionStatus::Idle => theme.text_ghost,
        SessionStatus::Connecting | SessionStatus::Working => theme.accent,
        SessionStatus::Waiting => theme.warning,
        SessionStatus::Failed => theme.danger,
    }
}

pub fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Ready",
        SessionStatus::Connecting => "Connecting",
        SessionStatus::Working => "Working",
        SessionStatus::Waiting => "Needs input",
        SessionStatus::Failed => "Stopped",
    }
}

pub fn activity_icon(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Reasoning => "icons/sparkle.svg",
        ActivityKind::Command => "icons/terminal.svg",
        ActivityKind::FileChange => "icons/pencil.svg",
        ActivityKind::Search => "icons/search.svg",
        ActivityKind::Plan => "icons/list.svg",
        ActivityKind::Tool => "icons/wrench.svg",
    }
}

pub fn activity_noun(kind: ActivityKind) -> (&'static str, &'static str) {
    match kind {
        ActivityKind::Reasoning => ("thought", "thoughts"),
        ActivityKind::Command => ("command", "commands"),
        ActivityKind::FileChange => ("file edit", "file edits"),
        ActivityKind::Search => ("search", "searches"),
        ActivityKind::Plan => ("plan step", "plan steps"),
        ActivityKind::Tool => ("tool call", "tool calls"),
    }
}

/// Uppercase micro-label used for sidebar sections.
pub fn section_label(theme: &Theme, label: &'static str) -> Div {
    div()
        .h(px(24.0))
        .flex()
        .items_center()
        .px_2()
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_ghost)
        .child(SharedString::from(label.to_ascii_uppercase()))
}

/// A compact composer chip that can act as a `gpui-component` dropdown-menu
/// trigger while keeping Waku's own visual language. The library only asks
/// its triggers to be styled, selectable, interactive elements — `selected`
/// is driven by the menu's open state, which we render as a soft fill.
#[derive(IntoElement)]
pub struct MenuChip {
    base: Stateful<Div>,
    icon: Option<(&'static str, Hsla)>,
    label: SharedString,
    caret: bool,
    selected: bool,
}

impl MenuChip {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id),
            icon: None,
            label: SharedString::default(),
            caret: true,
            selected: false,
        }
    }

    pub fn icon(mut self, path: &'static str, color: Hsla) -> Self {
        self.icon = Some((path, color));
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }
}

impl Styled for MenuChip {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for MenuChip {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl Selectable for MenuChip {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl DropdownMenu for MenuChip {}

impl RenderOnce for MenuChip {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let theme = Theme::dark();
        self.base
            .h(px(24.0))
            .px(px(7.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(11.5))
            .line_height(px(14.0))
            .cursor_pointer()
            .when(self.selected, |element| element.bg(theme.overlay))
            .hover(|element| element.bg(theme.overlay))
            .when_some(self.icon, |element, (path, color)| {
                element.child(icon(path, 10.5, color))
            })
            .child(div().text_color(theme.text_secondary).child(self.label))
            .when(self.caret, |element| {
                element.child(icon("icons/chevron-down.svg", 9.0, theme.text_ghost))
            })
    }
}

/// Small bordered keycap chip, e.g. `⌘N`.
pub fn key_hint(theme: &Theme, keys: &'static str) -> Div {
    div()
        .h(px(17.0))
        .px(px(5.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.border)
        .text_size(px(9.5))
        .line_height(px(11.0))
        .text_color(theme.text_ghost)
        .child(SharedString::from(keys))
}

/// "5m", "2h", "3d", then "Jul 12" for anything older than a week.
pub fn relative_time(timestamp: u64) -> String {
    let now = unix_time();
    let elapsed = now.saturating_sub(timestamp);
    if elapsed < 60 {
        return "now".into();
    }
    if elapsed < 3600 {
        return format!("{}m", elapsed / 60);
    }
    if elapsed < 86_400 {
        return format!("{}h", elapsed / 3600);
    }
    if elapsed < 7 * 86_400 {
        return format!("{}d", elapsed / 86_400);
    }
    let (_, month, day) = civil_from_unix(timestamp);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {}", MONTHS[(month - 1) as usize], day)
}

/// Gregorian civil date from a unix timestamp (Howard Hinnant's algorithm).
fn civil_from_unix(timestamp: u64) -> (i64, u32, u32) {
    let days = (timestamp / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineSpan {
    Code,
    Bold,
}

/// Strips a minimal inline-markdown subset (`code`, **bold**) out of `text`,
/// returning the cleaned string plus byte ranges to highlight. Unmatched
/// markers are kept literally.
pub fn parse_inline_markdown(text: &str) -> (String, Vec<(Range<usize>, InlineSpan)>) {
    let mut cleaned = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            if let Some(length) = text[index + 1..].find('`') {
                let start = cleaned.len();
                cleaned.push_str(&text[index + 1..index + 1 + length]);
                spans.push((start..cleaned.len(), InlineSpan::Code));
                index += length + 2;
                continue;
            }
        }
        if bytes[index] == b'*' && index + 1 < bytes.len() && bytes[index + 1] == b'*' {
            if let Some(length) = text[index + 2..].find("**") {
                let start = cleaned.len();
                cleaned.push_str(&text[index + 2..index + 2 + length]);
                spans.push((start..cleaned.len(), InlineSpan::Bold));
                index += length + 4;
                continue;
            }
        }
        let character = text[index..].chars().next().expect("char at index");
        cleaned.push(character);
        index += character.len_utf8();
    }
    (cleaned, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markdown_extracts_code_and_bold() {
        let (cleaned, spans) = parse_inline_markdown("run `cargo test` and **verify** it");
        assert_eq!(cleaned, "run cargo test and verify it");
        assert_eq!(spans.len(), 2);
        assert_eq!(&cleaned[spans[0].0.clone()], "cargo test");
        assert_eq!(spans[0].1, InlineSpan::Code);
        assert_eq!(&cleaned[spans[1].0.clone()], "verify");
        assert_eq!(spans[1].1, InlineSpan::Bold);
    }

    #[test]
    fn inline_markdown_keeps_unmatched_markers() {
        let (cleaned, spans) = parse_inline_markdown("a ` lonely backtick and ** stars");
        assert_eq!(cleaned, "a ` lonely backtick and ** stars");
        assert!(spans.is_empty());
    }

    #[test]
    fn inline_markdown_is_unicode_safe() {
        let (cleaned, spans) = parse_inline_markdown("日本語 `コード` テキスト");
        assert_eq!(cleaned, "日本語 コード テキスト");
        assert_eq!(&cleaned[spans[0].0.clone()], "コード");
    }

    #[test]
    fn relative_time_formats_buckets() {
        let now = unix_time();
        assert_eq!(relative_time(now), "now");
        assert_eq!(relative_time(now - 90), "1m");
        assert_eq!(relative_time(now - 7200), "2h");
        assert_eq!(relative_time(now - 3 * 86_400), "3d");
        assert!(relative_time(now - 30 * 86_400).contains(' '));
    }

    #[test]
    fn civil_date_is_correct() {
        // 2026-07-31 00:00:00 UTC
        let (year, month, day) = civil_from_unix(1_785_456_000);
        assert_eq!((year, month, day), (2026, 7, 31));
    }

    #[test]
    fn every_referenced_icon_is_embedded() {
        use crate::assets::Assets;
        use crate::model::{ActivityKind, ProviderKind};
        use gpui::AssetSource;

        let mut paths = vec![
            "icons/panel-left.svg",
            "icons/plus.svg",
            "icons/arrow-up.svg",
            "icons/stop.svg",
            "icons/check.svg",
            "icons/git-branch.svg",
            "icons/chevron-down.svg",
            "icons/chevron-right.svg",
            "icons/folder.svg",
            "icons/alert.svg",
            "icons/sparkle.svg",
        ];
        for provider in ProviderKind::ALL {
            paths.push(provider_icon(provider));
        }
        for kind in [
            ActivityKind::Reasoning,
            ActivityKind::Command,
            ActivityKind::FileChange,
            ActivityKind::Search,
            ActivityKind::Plan,
            ActivityKind::Tool,
        ] {
            paths.push(activity_icon(kind));
        }
        for path in paths {
            assert!(
                Assets.load(path).unwrap().is_some(),
                "missing embedded icon: {path}"
            );
        }
    }
}
