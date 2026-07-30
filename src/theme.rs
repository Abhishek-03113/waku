use gpui::{Hsla, hsla, rgb};

#[derive(Clone, Copy)]
pub struct Theme {
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub panel: Hsla,
    pub panel_hover: Hsla,
    pub elevated: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub code: Hsla,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            canvas: rgb(0x101113).into(),
            sidebar: rgb(0x151619).into(),
            panel: rgb(0x191b1f).into(),
            panel_hover: rgb(0x202329).into(),
            elevated: rgb(0x24272d).into(),
            border: hsla(220.0 / 360.0, 0.08, 0.22, 0.72),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.33, 0.88),
            text: rgb(0xf2f2f0).into(),
            text_muted: rgb(0xa1a39f).into(),
            text_faint: rgb(0x686b70).into(),
            accent: rgb(0xc8f169).into(),
            accent_soft: hsla(79.0 / 360.0, 0.82, 0.68, 0.12),
            success: rgb(0x6ee7a5).into(),
            warning: rgb(0xf4c46a).into(),
            danger: rgb(0xff7b72).into(),
            code: rgb(0x0c0d0f).into(),
        }
    }
}
