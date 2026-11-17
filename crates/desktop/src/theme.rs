use gpui::{App, Hsla, Window, px, rgb};
use gpui_component::{Theme, ThemeMode};

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub accent_text: Hsla,
    pub selected: Hsla,
    pub warning: Hsla,
}
impl Palette {
    pub fn new(dark: bool) -> Self {
        let colors = if dark {
            [
                0x10161d, 0x121a23, 0x151e28, 0x1c2935, 0x2b3947, 0xe9eff5, 0x9baaba, 0x55dbc8,
                0x102724, 0x183a3b, 0xe8bd78,
            ]
        } else {
            [
                0xf7f9fb, 0xeff3f6, 0xffffff, 0xe9eff3, 0xd6dfe6, 0x1b2c3a, 0x576b7b, 0x0d8175,
                0xffffff, 0xe1f3ef, 0x8f641d,
            ]
        };
        let [
            canvas,
            sidebar,
            surface,
            raised,
            border,
            text,
            muted,
            accent,
            accent_text,
            selected,
            warning,
        ] = colors.map(|c| rgb(c).into());
        Self {
            canvas,
            sidebar,
            surface,
            raised,
            border,
            text,
            muted,
            accent,
            accent_text,
            selected,
            warning,
        }
    }
}
pub fn configure(dark: bool, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        window,
        cx,
    );
    let p = Palette::new(dark);
    let theme = Theme::global_mut(cx);
    theme.font_family = "Segoe UI".into();
    theme.font_size = px(14.);
    theme.mono_font_family = "Cascadia Mono".into();
    theme.radius = px(6.);
    theme.shadow = false;
    theme.colors.background = p.surface;
    theme.colors.foreground = p.text;
    theme.colors.border = p.border;
    theme.colors.input = p.border;
    theme.colors.caret = p.accent;
    theme.colors.ring = p.accent;
    theme.colors.primary = p.accent;
    theme.colors.primary_hover = p.accent;
    theme.colors.primary_active = p.accent;
    theme.colors.primary_foreground = p.accent_text;
    theme.colors.secondary = p.raised;
    theme.colors.secondary_foreground = p.text;
    theme.colors.secondary_hover = p.selected;
    theme.colors.muted_foreground = p.muted;
    theme.colors.accent = p.raised;
    theme.colors.selection = p.selected;
    theme.colors.title_bar = p.sidebar;
    theme.colors.title_bar_border = p.border;
    theme.colors.muted = p.raised;
}
