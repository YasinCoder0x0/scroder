//! The Scroder look: one dark "studio" theme, Inter typography and the
//! #484BFF brand colour.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Shadow, Stroke};

pub struct Palette {
    pub bg: Color32,
    pub panel_top: Color32,
    pub surface: Color32,
    pub surface_elevated: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    pub input_bg: Color32,
    pub border: Color32,
    pub border_soft: Color32,
    pub border_hi: Color32,
    pub text: Color32,
    pub text_soft: Color32,
    pub muted: Color32,
    pub muted_dim: Color32,
    pub accent: Color32,
    pub accent_hi: Color32,
    pub accent_soft: Color32,
    pub danger: Color32,
    pub danger_hi: Color32,
    pub danger_soft: Color32,
    pub danger_border: Color32,
    pub danger_text: Color32,
    pub success: Color32,
    pub success_hi: Color32,
    pub success_soft: Color32,
    pub success_text: Color32,
    pub on_success: Color32,
    pub caution: Color32,
    pub caution_soft: Color32,
    pub toggle_off: Color32,
    pub toggle_off_hover: Color32,
    pub knob_off: Color32,
    pub focus: Color32,
    pub card_shadow: Shadow,
}

/// Slate surfaces (not pure black) keep panels, controls and video apart;
/// text is near-white for legibility.
pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(28, 31, 37),
    panel_top: Color32::from_rgb(35, 39, 46),
    surface: Color32::from_rgb(40, 44, 52),
    surface_elevated: Color32::from_rgb(52, 57, 67),
    surface_hover: Color32::from_rgb(62, 68, 80),
    surface_active: Color32::from_rgb(70, 77, 91),
    input_bg: Color32::from_rgb(22, 24, 29),
    border: Color32::from_rgb(58, 64, 75),
    border_soft: Color32::from_rgb(47, 52, 61),
    border_hi: Color32::from_rgb(82, 90, 105),
    text: Color32::from_rgb(244, 246, 249),
    text_soft: Color32::from_rgb(222, 226, 233),
    muted: Color32::from_rgb(184, 190, 201),
    muted_dim: Color32::from_rgb(150, 157, 170),
    accent: Color32::from_rgb(72, 75, 255),
    accent_hi: Color32::from_rgb(106, 109, 255),
    accent_soft: Color32::from_rgb(43, 45, 102),
    danger: Color32::from_rgb(232, 68, 74),
    danger_hi: Color32::from_rgb(244, 92, 97),
    danger_soft: Color32::from_rgb(60, 33, 37),
    danger_border: Color32::from_rgb(112, 52, 57),
    danger_text: Color32::from_rgb(255, 150, 155),
    success: Color32::from_rgb(52, 199, 105),
    success_hi: Color32::from_rgb(74, 214, 124),
    success_soft: Color32::from_rgb(30, 60, 42),
    success_text: Color32::from_rgb(110, 224, 150),
    on_success: Color32::from_rgb(8, 20, 11),
    caution: Color32::from_rgb(245, 190, 80),
    caution_soft: Color32::from_rgb(62, 50, 28),
    toggle_off: Color32::from_rgb(78, 85, 99),
    toggle_off_hover: Color32::from_rgb(92, 100, 116),
    knob_off: Color32::from_rgb(236, 239, 244),
    focus: Color32::from_rgb(120, 123, 255),
    card_shadow: Shadow::NONE,
};

/// Corner radii: compact and consistent, like professional desktop tools.
pub const RADIUS: u8 = 4;
pub const RADIUS_PANEL: u8 = 6;

pub fn pal() -> &'static Palette {
    &DARK
}

/// The SemiBold Inter face, for headings and emphasis.
pub fn bold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter-medium".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../resources/fonts/Inter-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../resources/fonts/Inter-SemiBold.ttf"
        ))),
    );

    // Keep egui's bundled fonts as fallbacks for symbols and emoji.
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut proportional = vec!["inter-medium".to_string()];
    proportional.extend(fallbacks.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Proportional, proportional);

    let mut semibold = vec!["inter-semibold".to_string()];
    semibold.extend(fallbacks);
    fonts.families.insert(bold(), semibold);
    fonts
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals(&DARK));
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::Vec2::new(8.0, 8.0);
        style.spacing.button_padding = egui::Vec2::new(10.0, 6.0);
        style.spacing.interact_size = egui::Vec2::new(40.0, 30.0);
        style.spacing.slider_width = 180.0;
        style.spacing.combo_width = 160.0;
        style.spacing.scroll.floating = true;
        for (text_style, font) in style.text_styles.iter_mut() {
            font.size = match text_style {
                egui::TextStyle::Small => 11.0,
                egui::TextStyle::Body | egui::TextStyle::Button => 13.0,
                egui::TextStyle::Monospace => 12.5,
                egui::TextStyle::Heading => 18.0,
                _ => font.size,
            };
        }
    });
}

fn visuals(p: &Palette) -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    let r = CornerRadius::same(RADIUS);
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(RADIUS_PANEL);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    v.menu_corner_radius = r;
    v.override_text_color = Some(p.text);
    v.selection.bg_fill = Color32::from_rgb(52, 54, 140);
    v.selection.stroke = Stroke::new(1.0, p.accent_hi);
    v.hyperlink_color = p.accent_hi;
    v.warn_fg_color = p.caution;
    v.error_fg_color = p.danger;
    v.extreme_bg_color = p.input_bg;
    v.text_edit_bg_color = Some(p.input_bg);
    v.code_bg_color = p.surface_elevated;
    v.faint_bg_color = p.surface_elevated;
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.toggle_off;
    w.inactive.weak_bg_fill = p.surface_elevated;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.accent_hi;
    w.hovered.weak_bg_fill = p.surface_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_hi);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;

    w.active.bg_fill = p.accent;
    w.active.weak_bg_fill = p.surface_active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;

    w.open.bg_fill = p.surface_elevated;
    w.open.weak_bg_fill = p.surface_elevated;
    w.open.bg_stroke = Stroke::new(1.0, p.border_hi);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = r;
    v
}
