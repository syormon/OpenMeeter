//! Custom-painted controls. Each takes an absolute `Rect` (the skin is a fixed
//! layout) and mutates the value it controls in place.

use std::f32::consts::{FRAC_PI_2, PI};

use eframe::egui::{
    Align2, Color32, CornerRadius, Event, Id, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    epaint::TextShape, pos2, vec2,
};

use super::ballistics::MeterLevel;
use super::theme::{self, font};
use crate::model::{MAX_GAIN_DB, MIN_GAIN_DB};

const METER_FLOOR_DB: f32 = -60.0;

pub fn text(painter: &Painter, pos: Pos2, align: Align2, s: &str, size: f32, color: Color32) -> Rect {
    painter.text(pos, align, s, font(size), color)
}

/// Draw each line of `s` centred on `center`.
pub fn text_lines_centered(painter: &Painter, center: Pos2, s: &str, size: f32, color: Color32) {
    let lines: Vec<&str> = s.lines().collect();
    let line_h = size + 1.0;
    let top = center.y - line_h * lines.len() as f32 / 2.0 + line_h / 2.0;
    for (i, line) in lines.iter().enumerate() {
        text(painter, pos2(center.x, top + i as f32 * line_h), Align2::CENTER_CENTER, line, size, color);
    }
}

/// Rounded outline toggle button, lit in `accent` when `on`.
pub fn button(ui: &mut Ui, rect: Rect, id: Id, label: &str, on: bool, accent: Color32) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    paint_button(ui, rect, &resp, on, accent);
    let color = if on { accent } else { theme::TEXT_DIM };
    text_lines_centered(ui.painter(), rect.center(), label, 13.0, color);
    resp
}

fn paint_button(ui: &Ui, rect: Rect, resp: &Response, on: bool, accent: Color32) {
    let painter = ui.painter();
    let radius = CornerRadius::same(6);
    let (fill, stroke) = match (on, resp.hovered()) {
        (true, _) => (accent.gamma_multiply(0.12), Stroke::new(1.5, accent)),
        (false, true) => (theme::PANEL, Stroke::new(1.2, theme::TEXT_DIM)),
        (false, false) => (Color32::TRANSPARENT, Stroke::new(1.0, theme::OUTLINE)),
    };
    painter.rect_filled(rect, radius, fill);
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
}

/// Bus routing button ("▶A1"), with the triangle painted rather than a glyph.
pub fn route_button(ui: &mut Ui, rect: Rect, id: Id, bus: &str, on: bool) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    paint_button(ui, rect, &resp, on, theme::GREEN);
    let color = if on { theme::GREEN } else { theme::TEXT_DIM };
    let painter = ui.painter();
    let c = rect.center();
    let tri_x = c.x - 12.0;
    painter.add(Shape::convex_polygon(
        vec![pos2(tri_x - 3.0, c.y - 4.5), pos2(tri_x + 4.0, c.y), pos2(tri_x - 3.0, c.y + 4.5)],
        color,
        Stroke::NONE,
    ));
    text(painter, pos2(c.x + 4.0, c.y), Align2::CENTER_CENTER, bus, 14.0, color);
    resp
}

/// Rotary knob. Drag vertically to change, double-click to reset.
#[allow(clippy::too_many_arguments)]
pub fn knob(
    ui: &mut Ui,
    center: Pos2,
    radius: f32,
    id: Id,
    value: &mut f32,
    range: (f32, f32),
    default: f32,
    show_value: bool,
) -> Response {
    let rect = Rect::from_center_size(center, vec2(radius * 2.0, radius * 2.0));
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let (lo, hi) = range;
    if resp.dragged() {
        *value = (*value - resp.drag_delta().y * (hi - lo) / 150.0).clamp(lo, hi);
    }
    if resp.double_clicked() {
        *value = default;
    }
    scroll_adjust(ui, &resp, value, range, (hi - lo) / 40.0);

    let painter = ui.painter();
    let ring = if resp.hovered() || resp.dragged() { theme::TEXT_DIM } else { theme::OUTLINE };
    painter.circle(center, radius, theme::PANEL, Stroke::new(2.0, ring));
    // 270° sweep, starting bottom-left.
    let t = (*value - lo) / (hi - lo);
    let angle = -0.75 * PI + t * 1.5 * PI;
    let dot = center + vec2(angle.sin(), -angle.cos()) * (radius - 6.0);
    painter.circle_filled(dot, 3.0, theme::TEXT);
    if show_value {
        text(painter, center, Align2::CENTER_CENTER, &format!("{:.0}", value), 11.0, theme::TEXT_DIM);
    }
    resp.on_hover_text(format!("{value:.1}"))
}

fn scroll_adjust(ui: &Ui, resp: &Response, value: &mut f32, (lo, hi): (f32, f32), step: f32) {
    if resp.hovered() {
        let scroll: f32 = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel { delta, .. } => Some(delta.y),
                    _ => None,
                })
                .sum()
        });
        if scroll != 0.0 {
            *value = (*value + scroll.signum() * step).clamp(lo, hi);
        }
    }
}

/// Voicemeeter-style fader: mint track, round thumb showing the gain.
pub fn fader(ui: &mut Ui, rect: Rect, id: Id, gain_db: &mut f32, muted: bool) -> Response {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let thumb_r = (rect.width() / 2.0 - 2.0).min(24.0);
    let travel_top = rect.top() + thumb_r;
    let travel = rect.height() - thumb_r * 2.0;
    let range = MAX_GAIN_DB - MIN_GAIN_DB;

    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        *gain_db -= resp.drag_delta().y / travel * range * fine;
        *gain_db = gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    }
    if resp.double_clicked() {
        *gain_db = 0.0;
    }
    scroll_adjust(ui, &resp, gain_db, (MIN_GAIN_DB, MAX_GAIN_DB), 0.5);

    let painter = ui.painter();
    let cx = rect.center().x;
    let track_w = thumb_r * 0.95;
    let track = Rect::from_min_max(pos2(cx - track_w / 2.0, rect.top() + 2.0), pos2(cx + track_w / 2.0, rect.bottom() - 2.0));
    let track_color = if muted { theme::FADER_TRACK_DIM } else { theme::FADER_TRACK };
    painter.rect_filled(track, CornerRadius::same(track_w as u8 / 2), track_color);

    // Rotated caption along the lower part of the track.
    let galley = painter.layout_no_wrap("Fader Gain".into(), font(15.0), theme::BG);
    let pos = pos2(cx - galley.size().y / 2.0, track.bottom() - 8.0);
    painter.add(TextShape::new(pos, galley, theme::BG).with_angle(-FRAC_PI_2));

    let t = (*gain_db - MIN_GAIN_DB) / range;
    let thumb = pos2(cx, travel_top + (1.0 - t) * travel);
    painter.circle_filled(thumb, thumb_r, theme::FADER_THUMB_RING);
    painter.circle_filled(thumb, thumb_r - 4.0, theme::FADER_THUMB);
    let label = if gain_db.abs() < 0.05 { "0dB".to_string() } else { format!("{gain_db:.1}") };
    text(painter, thumb, Align2::CENTER_CENTER, &label, 12.0, Color32::WHITE);

    resp.on_hover_text("Drag (Shift = fine), scroll, double-click = 0 dB")
}

/// Segmented LED meter, one column per channel, with a peak-hold segment.
/// Levels come pre-smoothed from [`super::ballistics`]; unknown channels show as silent.
pub fn meter(painter: &Painter, rect: Rect, levels: &[MeterLevel]) {
    const SEG_H: f32 = 3.0;
    const GAP: f32 = 1.0;
    let channels = levels.len().max(2);
    let col_w = (rect.width() - GAP * (channels as f32 - 1.0)) / channels as f32;
    let segments = ((rect.height() + GAP) / (SEG_H + GAP)).floor() as usize;

    for ch in 0..channels {
        let MeterLevel { level, hold, .. } = levels.get(ch).copied().unwrap_or_default();
        let hold_seg = (hold * segments as f32) as usize;
        let x = rect.left() + ch as f32 * (col_w + GAP);
        for seg in 0..segments {
            let frac = (seg as f32 + 0.5) / segments as f32;
            let y = rect.bottom() - (seg as f32 + 1.0) * (SEG_H + GAP) + GAP;
            let r = Rect::from_min_size(pos2(x, y), vec2(col_w, SEG_H));
            let lit = frac <= level || (hold > 0.0 && seg == hold_seg.min(segments - 1));
            let color = if lit { segment_color(frac) } else { theme::METER_OFF };
            painter.rect_filled(r, 0.0, color);
        }
    }
}

fn segment_color(frac: f32) -> Color32 {
    match frac {
        f if f > 0.95 => theme::METER_RED,
        f if f > 0.8 => theme::METER_YELLOW,
        _ => theme::METER_GREEN,
    }
}

/// Map a linear peak onto the meter's -60..0 dB scale.
pub fn level_fraction(peak: f32) -> f32 {
    if peak <= 0.0 {
        return 0.0;
    }
    ((20.0 * peak.log10() - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

/// 2D pad with a square handle. Both axes 0..1, y pointing up.
pub fn xy_pad(ui: &mut Ui, rect: Rect, id: Id, pos: &mut [f32; 2], default: [f32; 2]) -> Response {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let inner = rect.shrink(10.0);
    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        pos[0] = ((p.x - inner.left()) / inner.width()).clamp(0.0, 1.0);
        pos[1] = ((inner.bottom() - p.y) / inner.height()).clamp(0.0, 1.0);
    }
    if resp.double_clicked() {
        *pos = default;
    }

    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), theme::PANEL);
    painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, theme::PANEL_STROKE), StrokeKind::Inside);
    let grid = Stroke::new(1.0, theme::SEPARATOR);
    painter.line_segment([pos2(rect.center().x, inner.top()), pos2(rect.center().x, inner.bottom())], grid);
    painter.line_segment([pos2(inner.left(), rect.center().y), pos2(inner.right(), rect.center().y)], grid);

    let handle = pos2(inner.left() + pos[0] * inner.width(), inner.bottom() - pos[1] * inner.height());
    let handle_rect = Rect::from_center_size(handle, vec2(14.0, 14.0));
    painter.rect_filled(handle_rect, CornerRadius::same(3), Color32::from_rgb(140, 170, 195));
    resp
}

#[cfg(test)]
mod tests {
    use super::level_fraction;

    #[test]
    fn meter_scale_endpoints() {
        assert_eq!(level_fraction(0.0), 0.0);
        assert_eq!(level_fraction(1.0), 1.0);
        assert!(level_fraction(0.001).abs() < 1e-6); // -60 dB
        assert!((level_fraction(10f32.powf(-30.0 / 20.0)) - 0.5).abs() < 1e-4);
    }
}
