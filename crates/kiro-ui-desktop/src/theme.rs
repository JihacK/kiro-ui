//! Colour palette (same values as the web UI's CSS variables) applied to egui.

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub border: Color32,
    pub border_hover: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub green: Color32,
    pub yellow: Color32,
    pub red: Color32,
    pub code_bg: Color32,
    pub ghosts: [Color32; 6],
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Palette = Palette {
    bg: hex(0x131313),
    surface: hex(0x1a1a1a),
    surface2: hex(0x222222),
    border: hex(0x2e2e2e),
    border_hover: hex(0x3d3d3d),
    text: hex(0xf0f0f0),
    muted: hex(0x9a9a9a),
    dim: hex(0x5a5a5a),
    accent: hex(0x9046ff),
    accent_soft: Color32::from_rgba_premultiplied(17, 8, 31, 31),
    green: hex(0x34d399),
    yellow: hex(0xfbbf24),
    red: hex(0xfb7185),
    code_bg: hex(0x111111),
    ghosts: [hex(0x9046ff), hex(0x22d3ee), hex(0xfb923c), hex(0xf472b6), hex(0x34d399), hex(0xfb7185)],
};

pub const LIGHT: Palette = Palette {
    bg: hex(0xf7f7f8),
    surface: hex(0xffffff),
    surface2: hex(0xf0f0f2),
    border: hex(0xe4e4e7),
    border_hover: hex(0xd1d1d6),
    text: hex(0x18181b),
    muted: hex(0x71717a),
    dim: hex(0xa1a1aa),
    accent: hex(0x7c3aed),
    accent_soft: Color32::from_rgba_premultiplied(10, 5, 19, 20),
    green: hex(0x059669),
    yellow: hex(0xd97706),
    red: hex(0xe11d48),
    code_bg: hex(0xfafafa),
    ghosts: [hex(0x7c3aed), hex(0x0891b2), hex(0xea580c), hex(0xdb2777), hex(0x059669), hex(0xe11d48)],
};

pub fn palette(dark: bool) -> &'static Palette {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn apply(ctx: &egui::Context, dark: bool) {
    let p = palette(dark);
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface2;
    v.faint_bg_color = p.surface;
    v.code_bg_color = p.code_bg;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.yellow;
    v.error_fg_color = p.red;
    v.override_text_color = Some(p.text);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.selection.bg_fill = p.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    let r = CornerRadius::same(6);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_fill = p.surface;
    v.widgets.noninteractive.weak_bg_fill = p.surface;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.bg_fill = p.surface2;
    v.widgets.inactive.weak_bg_fill = p.surface2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.bg_fill = p.border;
    v.widgets.hovered.weak_bg_fill = p.border;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border_hover);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.active.bg_fill = p.border_hover;
    v.widgets.active.weak_bg_fill = p.border_hover;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.open.bg_fill = p.surface2;
    v.widgets.open.weak_bg_fill = p.surface2;
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.accent);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
    });
}
