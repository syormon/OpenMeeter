//! Colours and fonts for the Voicemeeter-style skin.

use eframe::egui::{self, Color32, FontId};

pub const BG: Color32 = Color32::from_rgb(47, 64, 80);
pub const PANEL: Color32 = Color32::from_rgb(36, 50, 63);
pub const PANEL_STROKE: Color32 = Color32::from_rgb(70, 90, 108);
pub const SEPARATOR: Color32 = Color32::from_rgb(62, 81, 99);

pub const TEXT: Color32 = Color32::from_rgb(200, 214, 225);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 160, 176);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(105, 125, 142);

pub const OUTLINE: Color32 = Color32::from_rgb(98, 118, 135);
pub const GREEN: Color32 = Color32::from_rgb(120, 222, 165);
pub const BLUE: Color32 = Color32::from_rgb(110, 205, 240);
pub const ORANGE: Color32 = Color32::from_rgb(245, 170, 70);
pub const RED: Color32 = Color32::from_rgb(235, 80, 70);

pub const FADER_TRACK: Color32 = Color32::from_rgb(126, 200, 160);
pub const FADER_TRACK_DIM: Color32 = Color32::from_rgb(88, 140, 112);
pub const FADER_THUMB_RING: Color32 = Color32::from_rgb(160, 225, 190);
pub const FADER_THUMB: Color32 = Color32::from_rgb(92, 160, 124);

pub const METER_OFF: Color32 = Color32::from_rgb(58, 34, 38);
pub const METER_GREEN: Color32 = Color32::from_rgb(90, 215, 120);
pub const METER_YELLOW: Color32 = Color32::from_rgb(235, 205, 60);
pub const METER_RED: Color32 = Color32::from_rgb(235, 70, 60);

pub const RECORDER_SCREEN: Color32 = Color32::from_rgb(218, 226, 212);

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.window_stroke.color = PANEL_STROKE;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = FADER_THUMB;
    ctx.set_visuals(visuals);
}
