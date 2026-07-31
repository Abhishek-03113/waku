use gpui::{App, Hsla, hsla, rgb};

/// Waku's visual language, take two: neutral graphite surfaces in the spirit
/// of Cursor — color is reserved for meaning. Layers go `canvas` (window +
/// sidebar) → `surface` (content pane) → `raised` (composer, bubbles, cards)
/// → `inset` (code wells). The coral accent appears only where the brand or
/// live activity earns it; everything else is a gray with a job.
#[derive(Clone, Copy)]
pub struct Theme {
    pub canvas: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub inset: Hsla,
    pub overlay: Hsla,
    pub overlay_strong: Hsla,

    pub border: Hsla,
    pub border_strong: Hsla,

    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_ghost: Hsla,

    /// Brand coral. Logo, caret, live-activity pulses — nothing structural.
    pub accent: Hsla,

    /// Light fill for primary buttons (send, allow), dark glyph on top.
    pub inverse: Hsla,
    pub on_inverse: Hsla,

    pub warning: Hsla,
    pub favorite: Hsla,
    pub danger: Hsla,
    pub danger_soft: Hsla,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            canvas: rgb(0x121316).into(),
            surface: rgb(0x1A1C20).into(),
            raised: rgb(0x24262C).into(),
            inset: rgb(0x0D0E11).into(),
            overlay: hsla(220.0 / 360.0, 0.10, 0.90, 0.05),
            overlay_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.09),

            border: hsla(220.0 / 360.0, 0.10, 0.90, 0.07),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.14),

            text: rgb(0xE9EAED).into(),
            text_secondary: rgb(0xA2A7B0).into(),
            text_tertiary: rgb(0x757B85).into(),
            text_ghost: rgb(0x50555D).into(),

            accent: rgb(0xE2795B).into(),

            inverse: rgb(0xE7E9EC).into(),
            on_inverse: rgb(0x17181C).into(),

            warning: rgb(0xE0B36A).into(),
            favorite: rgb(0xEAB308).into(),
            danger: rgb(0xE2726A).into(),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.63, 0.10),
        }
    }
}

/// Bridges Waku's palette into `gpui-component`'s global theme so its
/// components (popup menus, etc.) render in the same graphite language.
pub fn init_component_theme(cx: &mut App) {
    use gpui_component::theme::{Theme as ComponentTheme, ThemeMode};

    ComponentTheme::change(ThemeMode::Dark, None, cx);
    let ours = Theme::dark();
    let theme = ComponentTheme::global_mut(cx);
    theme.background = ours.surface;
    theme.foreground = ours.text;
    theme.popover = ours.raised;
    theme.popover_foreground = ours.text_secondary;
    theme.muted = ours.overlay;
    theme.muted_foreground = ours.text_tertiary;
    theme.accent = ours.overlay_strong;
    theme.accent_foreground = ours.text;
    theme.border = ours.border_strong;
}
