use eframe::egui::{self, emath::NumExt, epaint::Mesh, Color32, CornerRadius, Rect, Shape, Stroke};

// Meta-inspired palette: near-black surfaces, blue → violet gradient accent.
pub const BG: Color32 = Color32::from_rgb(8, 8, 12);
pub const PANEL: Color32 = Color32::from_rgb(17, 17, 23);
pub const ELEVATED: Color32 = Color32::from_rgb(24, 24, 32);
pub const ELEVATED_HOVER: Color32 = Color32::from_rgb(32, 32, 42);
pub const BORDER: Color32 = Color32::from_rgb(44, 44, 56);
pub const TEXT: Color32 = Color32::from_rgb(244, 244, 247);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(154, 154, 168);
pub const BLUE: Color32 = Color32::from_rgb(6, 102, 235);
pub const VIOLET: Color32 = Color32::from_rgb(131, 58, 246);
pub const PINK: Color32 = Color32::from_rgb(228, 58, 130);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, style_up);
}

fn style_up(style: &mut egui::Style) {
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.panel_fill = BG;
    v.faint_bg_color = ELEVATED;
    v.extreme_bg_color = Color32::from_rgb(4, 4, 6);
    v.selection.bg_fill = BLUE.gamma_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0, BLUE);
    v.hyperlink_color = BLUE;
    v.window_corner_radius = CornerRadius::same(16);
    v.menu_corner_radius = CornerRadius::same(12);

    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);

    v.widgets.inactive.bg_fill = ELEVATED;
    v.widgets.inactive.weak_bg_fill = ELEVATED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    v.widgets.hovered.bg_fill = ELEVATED_HOVER;
    v.widgets.hovered.weak_bg_fill = ELEVATED_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.2, BLUE);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);

    v.widgets.active.bg_fill = BLUE;
    v.widgets.active.weak_bg_fill = BLUE;
    v.widgets.active.bg_stroke = Stroke::new(1.0, VIOLET);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    v.widgets.open.bg_fill = ELEVATED_HOVER;
    v.widgets.open.weak_bg_fill = ELEVATED_HOVER;
    v.widgets.open.bg_stroke = Stroke::new(1.0, BLUE);

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(10);
    }

    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.window_margin = egui::Margin::same(12);
    style.spacing.menu_margin = egui::Margin::same(8);
}

/// Compact square icon button (~28px), no visible label — pair with
/// `.on_hover_text()` for the informative part without spending width on
/// text. Gradient fill when `active`, ghost outline otherwise.
pub fn icon_toggle(ui: &mut egui::Ui, active: bool, icon: &str) -> egui::Response {
    let size = egui::vec2(28.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        if active {
            gradient_rect(ui.painter(), rect, 8.0);
        } else {
            let fill = if hovered { ELEVATED_HOVER } else { ELEVATED };
            let stroke = if hovered { Stroke::new(1.2, BLUE) } else { Stroke::new(1.0, BORDER) };
            ui.painter().rect_filled(rect, 8.0, fill);
            ui.painter().rect_stroke(rect, 8.0, stroke, egui::StrokeKind::Inside);
        }
        let text_color = if active { Color32::WHITE } else { TEXT };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(14.0),
            text_color,
        );
    }
    response
}

/// Small pill chip (compact version of `pill_toggle`, for short labels like
/// language codes) — tighter padding, smaller font.
pub fn chip_toggle(ui: &mut egui::Ui, active: bool, text: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(12.5),
        TEXT,
    );
    let padding = egui::vec2(10.0, 5.0);
    let size = (galley.size() + padding * 2.0).at_least(egui::vec2(28.0, 26.0));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let radius = rect.height() / 2.0;
        if active {
            gradient_rect(ui.painter(), rect, radius);
        } else {
            let fill = if hovered { ELEVATED_HOVER } else { ELEVATED };
            let stroke = if hovered { Stroke::new(1.2, BLUE) } else { Stroke::new(1.0, BORDER) };
            ui.painter().rect_filled(rect, radius, fill);
            ui.painter().rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);
        }
        let text_color = if active { Color32::WHITE } else { TEXT };
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, text_color);
    }
    response
}

/// Paint a diagonal blue → violet gradient into `rect`, clipped to rounded
/// corners via a clip rect (egui meshes don't rasterize rounding directly).
pub fn gradient_rect(painter: &egui::Painter, rect: Rect, corner_radius: f32) {
    // background rounded shape first, so corners outside the mesh are covered
    painter.rect_filled(rect, corner_radius, BLUE);
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), BLUE);
    mesh.colored_vertex(rect.right_top(), VIOLET);
    mesh.colored_vertex(rect.right_bottom(), PINK);
    mesh.colored_vertex(rect.left_bottom(), VIOLET);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    let clip = painter.clip_rect().intersect(rect);
    painter.with_clip_rect(clip).add(Shape::mesh(mesh));
}
