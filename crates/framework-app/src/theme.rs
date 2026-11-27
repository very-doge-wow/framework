use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0x8d, 0xff);
pub const BG: Color32 = Color32::from_rgb(0x14, 0x15, 0x18);
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x22);
pub const LANE: Color32 = Color32::from_rgb(0x22, 0x25, 0x2b);
pub const VIDEO_CLIP: Color32 = Color32::from_rgb(0x3a, 0x6f, 0xd8);
pub const IMAGE_CLIP: Color32 = Color32::from_rgb(0x8a, 0x5c, 0xd6);
pub const AUDIO_CLIP: Color32 = Color32::from_rgb(0x2f, 0xa5, 0x7a);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0x5a, 0x5f);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xab);

/// Applies the dark Framework look to an egui context.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.hyperlink_color = ACCENT;
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}
