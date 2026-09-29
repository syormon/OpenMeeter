//! The skin's sections, laid out on a fixed design grid (see `mod.rs`).
//! All coordinates are relative to the design origin `o`.

use std::collections::HashMap;

use eframe::egui::{Align2, Color32, Id, Popup, Pos2, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use openmeeter_backend::{DeviceId, DeviceInfo, Direction, driver_name};

use super::ballistics::{Ballistics, MeterLevel};

use super::theme;
use super::widgets::{button, fader, knob, meter, route_button, text, xy_pad};
use crate::model::{BusMode, KARAOKE_MODES, Kind, PanMode, RecorderSettings, Strip, default_strip_label};
use crate::recorder::Display as RecorderDisplay;

pub const TITLE_H: f32 = 34.0;
pub const HEADER_BOTTOM: f32 = 92.0;
/// Baseline row of the strip titles ("HARDWARE INPUT 1", "VIRTUAL INPUTS").
const HEADER_TITLE_Y: f32 = 49.0;
/// Device names sit below the titles, wrapping onto a second line if needed.
const HEADER_NAME_Y: f32 = 60.0;
const HEADER_NAME_H: f32 = 30.0;
/// Separators inside a titled group start below the title.
pub const GROUP_SEPARATOR_TOP: f32 = 59.0;
pub const HW_STRIP_W: f32 = 160.0;
pub const VIRT_STRIP_W: f32 = 112.0;
pub const RIGHT_W: f32 = 320.0;
pub const DESIGN_H: f32 = 620.0;

const BTN_H: f32 = 25.0;
/// y positions of the strip button column: A1..A3, B1..B2, then three extras.
const ROUTE_Y: [f32; 5] = [348.0, 377.0, 406.0, 440.0, 469.0];
const EXTRA_Y: [f32; 3] = [507.0, 540.0, 571.0];
const FADER_TOP: f32 = 346.0;

pub struct Ctx<'a> {
    pub devices: &'a [DeviceInfo],
    pub meters: &'a Ballistics,
    /// Nodes whose device failed to start or stopped, keyed by node key.
    pub errors: &'a HashMap<String, String>,
    /// Cable driver name -> label of the B bus / virtual input using it. One cable
    /// can't be both, so each side's menu greys out the cables the other side holds.
    pub bus_cables: &'a HashMap<String, String>,
    pub strip_cables: &'a HashMap<String, String>,
    pub can_create_virtual: bool,
}

/// How a node's device binding is shown in the header.
struct DeviceLabel {
    text: String,
    color: Color32,
    /// Set when the chosen device isn't working; shown as red text plus this hover.
    problem: Option<String>,
}

impl Ctx<'_> {
    fn peaks(&self, key: &str) -> &[MeterLevel] {
        self.meters.get(key)
    }

    /// Red only when a device is chosen but missing or failing; otherwise normal text.
    fn device_label(&self, key: &str, id: &Option<DeviceId>, placeholder: &str) -> DeviceLabel {
        let Some(id) = id else {
            return DeviceLabel { text: placeholder.to_owned(), color: theme::TEXT_FAINT, problem: None };
        };
        match self.devices.iter().find(|d| &d.id == id) {
            None => DeviceLabel {
                text: "(missing device)".to_owned(),
                color: theme::RED,
                problem: Some("This device isn't connected. Plug it in and use Menu > Refresh devices.".to_owned()),
            },
            Some(d) => match self.errors.get(key) {
                Some(err) => DeviceLabel { text: d.name.clone(), color: theme::RED, problem: Some(err.clone()) },
                None => DeviceLabel { text: d.name.clone(), color: theme::TEXT, problem: None },
            },
        }
    }
}

fn r(o: Pos2, x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(o + vec2(x, y), vec2(w, h))
}

/// Device name under a strip title: wraps to two lines, then ends in an ellipsis.
fn header_name(p: &eframe::egui::Painter, rect: Rect, name: &str, color: eframe::egui::Color32) {
    let mut job = eframe::egui::text::LayoutJob::simple(name.to_owned(), theme::font(11.0), color, rect.width());
    job.wrap.max_rows = 2;
    job.wrap.overflow_character = Some('\u{2026}');
    let galley = p.layout_job(job);
    p.galley(rect.left_top(), galley, color);
}

fn vline(ui: &Ui, o: Pos2, x: f32, y0: f32, y1: f32) {
    ui.painter().line_segment([o + vec2(x, y0), o + vec2(x, y1)], Stroke::new(1.0, theme::SEPARATOR));
}

/// Popup listing devices to bind to, opened by clicking `resp`.
/// Popup listing devices to bind to, opened by clicking `resp`. Devices for which
/// `taken` returns a note are still selectable (the cable moves over); the note
/// is shown next to the name and on hover.
fn device_menu<'a>(
    resp: &Response,
    selected: &mut Option<DeviceId>,
    candidates: impl Iterator<Item = &'a DeviceInfo>,
    taken: impl Fn(&DeviceInfo) -> Option<String>,
) {
    Popup::menu(resp).show(|ui| {
        ui.set_min_width(260.0);
        if ui.selectable_label(selected.is_none(), "(none)").clicked() {
            *selected = None;
        }
        for d in candidates {
            let note = taken(d);
            let label = match &note {
                Some(_) => format!("{}   (in use, will move here)", d.name),
                None => d.name.clone(),
            };
            let resp = ui.add(eframe::egui::Button::selectable(selected.as_ref() == Some(&d.id), label));
            if resp.clicked() {
                *selected = Some(d.id.clone());
            }
            if let Some(note) = note {
                resp.on_hover_text(note);
            }
        }
    });
}

/// Right-click a strip's title to rename it, as in Voicemeeter.
fn rename_menu(resp: &Response, label: &mut String, default: String) {
    // Its own id: by default it shares the device menu's, and a context menu
    // closes itself on left clicks, which would shut the device menu as it opens.
    Popup::context_menu(resp).id(resp.id.with("rename")).show(|ui| {
        ui.label(RichText::new("Strip name").strong());
        ui.add(eframe::egui::TextEdit::singleline(label).char_limit(24).desired_width(180.0));
        if ui.add_enabled(*label != default, eframe::egui::Button::new("Reset name")).clicked() {
            *label = default;
        }
    });
}

/// Reason a cable can't be chosen because the other side (`users`) already holds it.
fn cable_taken(users: &HashMap<String, String>, device: &DeviceInfo) -> Option<String> {
    let driver = driver_name(&device.name)?;
    let user = users.get(driver)?;
    Some(format!(
        "{user} uses this cable now. One cable can be a virtual input or a B bus, not both, so choosing it here unassigns {user}."
    ))
}

fn never_taken(_: &DeviceInfo) -> Option<String> {
    None
}

fn candidates<'a>(ctx: &'a Ctx, direction: Direction, is_virtual: bool) -> impl Iterator<Item = &'a DeviceInfo> {
    ctx.devices.iter().filter(move |d| d.direction == direction && d.is_virtual == is_virtual)
}

fn route_buttons(ui: &mut Ui, o: Pos2, x: f32, w: f32, strip: &mut Strip, bus_keys: &[String]) {
    for (key, y) in bus_keys.iter().zip(ROUTE_Y) {
        let on = strip.routes.contains(key);
        let id = Id::new(("route", &strip.key, key));
        if route_button(ui, r(o, x, y, w, BTN_H), id, key, on).clicked() {
            if on {
                strip.routes.remove(key);
            } else {
                strip.routes.insert(key.clone());
            }
        }
    }
}

fn solo_mute(ui: &mut Ui, o: Pos2, x: f32, w: f32, strip: &mut Strip) {
    let solo = r(o, x, EXTRA_Y[1], w, BTN_H);
    if button(ui, solo, Id::new(("solo", &strip.key)), "solo", strip.solo, theme::ORANGE).clicked() {
        strip.solo = !strip.solo;
    }
    let mute = r(o, x, EXTRA_Y[2], w, BTN_H);
    if button(ui, mute, Id::new(("mute", &strip.key)), "Mute", strip.mute, theme::RED).clicked() {
        strip.mute = !strip.mute;
    }
}

pub fn hardware_strip(ui: &mut Ui, o: Pos2, x: f32, n: usize, strip: &mut Strip, bus_keys: &[String], ctx: &Ctx) {
    let p = ui.painter().clone();

    // Header: title + bound device (click to choose).
    let header = r(o, x, TITLE_H + 2.0, HW_STRIP_W, HEADER_BOTTOM - TITLE_H - 4.0);
    let resp = ui.interact(header, Id::new(("hw-header", &strip.key)), Sense::click());
    if strip.label == default_strip_label(Kind::Hardware, n) {
        let title_end = text(&p, o + vec2(x + 6.0, HEADER_TITLE_Y), Align2::LEFT_CENTER, "HARDWARE INPUT", 15.0, theme::TEXT);
        text(&p, pos2(title_end.right() + 5.0, title_end.center().y), Align2::LEFT_CENTER, &n.to_string(), 17.0, theme::TEXT);
    } else {
        // A name set in System Settings replaces the title.
        let title_rect = r(o, x + 6.0, HEADER_TITLE_Y - 10.0, HW_STRIP_W - 12.0, 20.0);
        let title = strip.label.to_uppercase();
        p.with_clip_rect(title_rect).text(title_rect.left_center(), Align2::LEFT_CENTER, title, theme::font(15.0), theme::TEXT);
    }
    let label = ctx.device_label(&strip.key, &strip.device, "Select Input Device");
    header_name(&p, r(o, x + 6.0, HEADER_NAME_Y, HW_STRIP_W - 12.0, HEADER_NAME_H), &label.text, label.color);
    let hover = label.problem.unwrap_or_else(|| "Choose input device (right-click to rename)".to_owned());
    let resp = resp.on_hover_text(hover);
    device_menu(&resp, &mut strip.device, candidates(ctx, Direction::Capture, false), never_taken);
    rename_menu(&resp, &mut strip.label, default_strip_label(Kind::Hardware, n));

    // Intellipan.
    text(&p, o + vec2(x + HW_STRIP_W / 2.0, 114.0), Align2::CENTER_CENTER, "INTELLIPAN", 13.0, theme::TEXT_DIM);
    let pad = r(o, x + 7.0, 128.0, HW_STRIP_W - 14.0, 128.0);
    xy_pad(ui, pad, Id::new(("pan", &strip.key)), &mut strip.pan, [0.5, 0.0]);
    let mode_rect = r(o, x + 12.0, 131.0, 100.0, 16.0);
    let mode = ui.interact(mode_rect, Id::new(("pan-mode", &strip.key)), Sense::click());
    text(&p, mode_rect.left_center(), Align2::LEFT_CENTER, strip.pan_mode.label(), 11.0, theme::TEXT_DIM);
    Popup::menu(&mode.on_hover_text("Intellipan mode")).show(|ui| {
        for m in PanMode::ALL {
            ui.selectable_value(&mut strip.pan_mode, m, m.label());
        }
    });

    // Comp / Audibility / Gate.
    text(&p, o + vec2(x + 10.0, 271.0), Align2::LEFT_CENTER, "Comp.", 11.0, theme::TEXT_DIM);
    text(&p, o + vec2(x + HW_STRIP_W / 2.0, 271.0), Align2::CENTER_CENTER, "AUDIBILITY", 13.0, theme::TEXT_DIM);
    text(&p, o + vec2(x + HW_STRIP_W - 10.0, 271.0), Align2::RIGHT_CENTER, "Gate", 11.0, theme::TEXT_DIM);
    let knob_box = r(o, x + 10.0, 281.0, HW_STRIP_W - 20.0, 56.0);
    p.rect_stroke(knob_box, 4.0, Stroke::new(1.0, theme::SEPARATOR), eframe::egui::StrokeKind::Inside);
    knob(ui, o + vec2(x + 45.0, 309.0), 21.0, Id::new(("comp", &strip.key)), &mut strip.comp, (0.0, 10.0), 0.0, true);
    knob(ui, o + vec2(x + 115.0, 309.0), 21.0, Id::new(("gate", &strip.key)), &mut strip.gate, (0.0, 10.0), 0.0, true);

    // Meter, fader, buttons.
    meter(&p, r(o, x + 8.0, 350.0, 32.0, 248.0), ctx.peaks(&strip.key));
    fader(ui, r(o, x + 46.0, FADER_TOP, 54.0, 254.0), Id::new(("fader", &strip.key)), &mut strip.gain_db, strip.mute);
    route_buttons(ui, o, x + 106.0, 46.0, strip, bus_keys);
    let mono = r(o, x + 106.0, EXTRA_Y[0], 46.0, BTN_H);
    if button(ui, mono, Id::new(("mono", &strip.key)), "mono", strip.mono, theme::BLUE).clicked() {
        strip.mono = !strip.mono;
    }
    solo_mute(ui, o, x + 106.0, 46.0, strip);
}

pub fn virtual_strip(ui: &mut Ui, o: Pos2, x: f32, j: usize, strip: &mut Strip, bus_keys: &[String], ctx: &Ctx) {
    let p = ui.painter().clone();

    // Header name (the "VIRTUAL INPUTS" title is drawn once by the caller).
    let name_rect = r(o, x + 6.0, HEADER_NAME_Y, VIRT_STRIP_W - 10.0, HEADER_NAME_H);
    let click_rect = r(o, x, HEADER_NAME_Y - 2.0, VIRT_STRIP_W, HEADER_BOTTOM - HEADER_NAME_Y);
    let resp = ui.interact(click_rect, Id::new(("v-header", &strip.key)), Sense::click());
    rename_menu(&resp, &mut strip.label, default_strip_label(Kind::Virtual, j + 1));
    if ctx.can_create_virtual {
        let device = crate::model::virtual_strip_device(&strip.key, j + 1).description;
        header_name(&p, name_rect, &device, theme::TEXT);
        resp.on_hover_text(format!("Apps play into this strip through the \"{device}\" output device (right-click to rename)"));
    } else {
        let label = ctx.device_label(&strip.key, &strip.device, "Select VB-Cable");
        if strip.label == default_strip_label(Kind::Virtual, j + 1) {
            header_name(&p, name_rect, &label.text, label.color);
        } else {
            let (top, bottom) = (name_rect.with_max_y(name_rect.min.y + 14.0), name_rect.with_min_y(name_rect.min.y + 14.0));
            p.with_clip_rect(top).text(top.left_top(), Align2::LEFT_TOP, &strip.label, theme::font(11.0), theme::TEXT);
            p.with_clip_rect(bottom).text(bottom.left_top(), Align2::LEFT_TOP, &label.text, theme::font(10.0), label.color);
        }
        let hover = label.problem.unwrap_or_else(|| "Choose the cable apps play into, e.g. CABLE Input (right-click to rename)".to_owned());
        let taken = |d: &DeviceInfo| cable_taken(ctx.bus_cables, d);
        device_menu(&resp.on_hover_text(hover), &mut strip.device, candidates(ctx, Direction::Playback, true), taken);
    }

    // Equalizer: treble top-left, mid right, bass bottom-left, as in Voicemeeter.
    text(&p, o + vec2(x + VIRT_STRIP_W / 2.0, 114.0), Align2::CENTER_CENTER, "EQUALIZER", 13.0, theme::TEXT_DIM);
    let eq = (-12.0, 12.0);
    let k = |name: &str| Id::new((name, strip.key.clone()));
    knob(ui, o + vec2(x + 30.0, 152.0), 18.0, k("treble"), &mut strip.eq_treble, eq, 0.0, false);
    text(&p, o + vec2(x + 84.0, 133.0), Align2::CENTER_CENTER, "Treble", 11.0, theme::BLUE);
    text(&p, o + vec2(x + 84.0, 152.0), Align2::CENTER_CENTER, &format!("{:.1}", strip.eq_treble), 14.0, theme::TEXT_DIM);
    knob(ui, o + vec2(x + 80.0, 193.0), 18.0, k("mid"), &mut strip.eq_mid, eq, 0.0, false);
    text(&p, o + vec2(x + 26.0, 193.0), Align2::CENTER_CENTER, &format!("{:.1}", strip.eq_mid), 14.0, theme::TEXT_DIM);
    knob(ui, o + vec2(x + 30.0, 234.0), 18.0, k("bass"), &mut strip.eq_bass, eq, 0.0, false);
    text(&p, o + vec2(x + 84.0, 234.0), Align2::CENTER_CENTER, &format!("{:.1}", strip.eq_bass), 14.0, theme::TEXT_DIM);
    text(&p, o + vec2(x + 84.0, 254.0), Align2::CENTER_CENTER, "Bass", 11.0, theme::BLUE);

    // Surround panner + small meter.
    let pad = r(o, x + 8.0, 274.0, 68.0, 64.0);
    xy_pad(ui, pad, Id::new(("surround", &strip.key)), &mut strip.surround, [0.5, 0.5]);
    text(&p, pad.center_top() + vec2(0.0, 9.0), Align2::CENTER_CENTER, "Front", 9.0, theme::TEXT_FAINT);
    text(&p, pad.center_bottom() - vec2(0.0, 9.0), Align2::CENTER_CENTER, "Rear", 9.0, theme::TEXT_FAINT);
    text(&p, pad.left_center() + vec2(8.0, 0.0), Align2::CENTER_CENTER, "L", 9.0, theme::TEXT_FAINT);
    text(&p, pad.right_center() - vec2(8.0, 0.0), Align2::CENTER_CENTER, "R", 9.0, theme::TEXT_FAINT);
    meter(&p, r(o, x + 84.0, 274.0, 18.0, 64.0), ctx.peaks(&strip.key));

    fader(ui, r(o, x + 6.0, FADER_TOP, 54.0, 254.0), Id::new(("fader", &strip.key)), &mut strip.gain_db, strip.mute);
    route_buttons(ui, o, x + 64.0, 42.0, strip, bus_keys);

    // Voicemeeter puts M.C on the first virtual strip and K (karaoke) on the second.
    let extra = r(o, x + 64.0, EXTRA_Y[0], 42.0, BTN_H);
    if j.is_multiple_of(2) {
        let resp = button(ui, extra, Id::new(("mc", &strip.key)), "M.C", strip.mix_centre, theme::BLUE);
        if resp.on_hover_text("Mix to centre").clicked() {
            strip.mix_centre = !strip.mix_centre;
        }
    } else {
        let label = if strip.karaoke == 0 { "K".to_string() } else { format!("K{}", strip.karaoke) };
        let resp = button(ui, extra, Id::new(("k", &strip.key)), &label, strip.karaoke > 0, theme::BLUE);
        if resp.on_hover_text("Karaoke mode (click to cycle)").clicked() {
            strip.karaoke = (strip.karaoke + 1) % (KARAOKE_MODES + 1);
        }
    }
    solo_mute(ui, o, x + 64.0, 42.0, strip);
}

pub fn virtual_inputs_title(ui: &Ui, o: Pos2, x: f32, w: f32) {
    let p = ui.painter();
    text(p, o + vec2(x + w / 2.0, HEADER_TITLE_Y), Align2::CENTER_CENTER, "VIRTUAL INPUTS", 15.0, theme::TEXT);
}

/// A1..An device selectors and the "HARDWARE OUT" device list.
pub fn hardware_out_header(ui: &mut Ui, o: Pos2, x: f32, buses: &mut [crate::model::Bus], ctx: &Ctx) {
    let p = ui.painter().clone();
    let hw: Vec<_> = buses.iter_mut().filter(|b| b.kind == Kind::Hardware).collect();
    let n = hw.len();
    let mut names = Vec::new();
    for (i, bus) in hw.into_iter().enumerate() {
        let rect = r(o, x + 10.0 + i as f32 * 32.0, 42.0, 28.0, 42.0);
        let bound = bus.device.is_some();
        let resp = ui.interact(rect, Id::new(("out-header", &bus.key)), Sense::click());
        let stroke = if resp.hovered() { theme::TEXT_DIM } else { theme::OUTLINE };
        p.rect_filled(rect, 4.0, theme::PANEL);
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, stroke), eframe::egui::StrokeKind::Inside);
        let color = if bound { theme::TEXT } else { theme::TEXT_DIM };
        text(&p, rect.center() - vec2(0.0, 7.0), Align2::CENTER_CENTER, &bus.key, 14.0, color);
        let c = rect.center() + vec2(0.0, 10.0);
        p.add(eframe::egui::Shape::convex_polygon(
            vec![c + vec2(-5.0, -3.0), c + vec2(5.0, -3.0), c + vec2(0.0, 3.0)],
            color,
            Stroke::NONE,
        ));
        let label = ctx.device_label(&bus.key, &bus.device, "");
        if bus.device.is_some() {
            let color = if label.problem.is_some() { theme::RED } else { theme::TEXT_DIM };
            names.push((format!("{}: {}", bus.key, label.text), color));
        }
        let hover = label.problem.unwrap_or_else(|| format!("Choose output device for {}", bus.key));
        device_menu(&resp.on_hover_text(hover), &mut bus.device, candidates(ctx, Direction::Playback, false), never_taken);
    }

    let text_x = x + 16.0 + n as f32 * 32.0;
    text(&p, o + vec2(text_x, 50.0), Align2::LEFT_CENTER, "HARDWARE OUT", 15.0, theme::TEXT);
    let list = r(o, text_x, 60.0, RIGHT_W - (text_x - x) - 6.0, 32.0);
    let clipped = p.with_clip_rect(list);
    for (i, (line, color)) in names.iter().take(3).enumerate() {
        clipped.text(list.left_top() + vec2(0.0, i as f32 * 10.5), Align2::LEFT_TOP, line, theme::font(10.0), *color);
    }
}

/// A transport or setup request from the recorder panel, handled by the app.
pub enum RecorderAction {
    Load,
    Rewind,
    Forward,
    PlayPause,
    Stop,
    Record,
    ChooseFolder,
    OpenFolder,
    OpenOptions,
}

/// Cassette-style recorder, as in Voicemeeter Banana. Click the screen to load a
/// file, right-click anywhere on it to open Recorder Options.
pub fn recorder(
    ui: &mut Ui,
    o: Pos2,
    x: f32,
    settings: &mut RecorderSettings,
    bus_keys: &[String],
    display: &RecorderDisplay,
) -> Option<RecorderAction> {
    let p = ui.painter().clone();
    let mut action = None;
    let dark = Color32::from_rgb(40, 50, 55);

    let body = r(o, x + 10.0, 104.0, 246.0, 168.0);
    let body_resp = ui.interact(body, Id::new("rec-body"), Sense::click());
    p.rect_filled(body, 10.0, Color32::from_rgb(26, 34, 42));
    if display.recording {
        p.rect_stroke(body, 10.0, Stroke::new(2.0, theme::RED), StrokeKind::Inside);
    }

    // Screen: click to load a file.
    let screen = r(o, x + 20.0, 114.0, 226.0, 42.0);
    let screen_resp = ui.interact(screen, Id::new("rec-screen"), Sense::click());
    p.rect_filled(screen, 4.0, theme::RECORDER_SCREEN);
    let title_color = if display.recording { Color32::from_rgb(190, 30, 30) } else { dark };
    let clip = p.with_clip_rect(screen.shrink(4.0));
    clip.text(screen.left_top() + vec2(6.0, 12.0), Align2::LEFT_CENTER, &display.title, theme::font(12.0), title_color);
    let detail_color = if display.error { Color32::from_rgb(190, 30, 30) } else { dark };
    clip.text(screen.left_top() + vec2(6.0, 30.0), Align2::LEFT_CENTER, &display.detail, theme::font(10.0), detail_color);
    if screen_resp.on_hover_text("Click to load an audio file (WAV, MP3, FLAC, OGG, M4A)").clicked() {
        action = Some(RecorderAction::Load);
    }

    // Tape window with reels that turn while playing or recording.
    let tape = r(o, x + 20.0, 162.0, 226.0, 60.0);
    p.rect_filled(tape, 4.0, Color32::from_rgb(48, 60, 72));
    let spin = if display.playing || display.recording { ui.input(|i| i.time) as f32 * 3.0 } else { 0.0 };
    for cx in [tape.left() + 36.0, tape.right() - 36.0] {
        let c = pos2(cx, tape.center().y);
        p.circle(c, 13.0, dark, Stroke::new(2.0, theme::OUTLINE));
        for k in 0..3 {
            let a = spin + k as f32 * std::f32::consts::TAU / 3.0;
            p.circle_filled(c + vec2(a.cos(), a.sin()) * 8.0, 2.0, theme::OUTLINE);
        }
    }
    text(&p, tape.center(), Align2::CENTER_CENTER, &display.time, 14.0, theme::TEXT);
    if display.playing || display.recording {
        ui.ctx().request_repaint();
    }

    // Transport.
    let buttons = [
        (TransportIcon::Rewind, "Back 10 seconds", RecorderAction::Rewind),
        (TransportIcon::Forward, "Forward 10 seconds", RecorderAction::Forward),
        (if display.playing { TransportIcon::Pause } else { TransportIcon::Play }, "Play / pause", RecorderAction::PlayPause),
        (TransportIcon::Stop, "Stop (also ends a recording)", RecorderAction::Stop),
        (TransportIcon::Record, "Record the selected buses (right-click for options)", RecorderAction::Record),
    ];
    for (i, (icon, hover, act)) in buttons.into_iter().enumerate() {
        let b = r(o, x + 20.0 + i as f32 * 45.4, 230.0, 43.0, 34.0);
        let resp = ui.interact(b, Id::new(("rec-transport", i)), Sense::click());
        let lit = match icon {
            TransportIcon::Record => display.recording,
            TransportIcon::Pause => true,
            _ => false,
        };
        let fill = match (lit, resp.hovered()) {
            (true, _) => Color32::from_rgb(80, 98, 116),
            (false, true) => Color32::from_rgb(68, 84, 100),
            (false, false) => Color32::from_rgb(58, 72, 86),
        };
        p.rect_filled(b, 3.0, fill);
        transport_icon(&p, b.center(), icon);
        if resp.on_hover_text(hover).clicked() {
            action = Some(act);
        }
    }

    // Where playback goes (Voicemeeter's recorder buttons).
    for (i, key) in bus_keys.iter().enumerate() {
        let b = r(o, x + 262.0, 112.0 + i as f32 * 31.0, 44.0, 26.0);
        let on = settings.playback_buses.contains(key);
        let resp = button(ui, b, Id::new(("rec-out", key)), key, on, theme::GREEN);
        if resp.on_hover_text(format!("Play loaded files into {key}")).clicked() {
            if on {
                settings.playback_buses.remove(key);
            } else {
                settings.playback_buses.insert(key.clone());
            }
        }
    }

    // Right-click anywhere on the recorder for its options, as in Voicemeeter.
    if body_resp.on_hover_text("Right-click for recorder options").secondary_clicked() {
        action = Some(RecorderAction::OpenOptions);
    }

    action
}

#[derive(Clone, Copy)]
enum TransportIcon {
    Rewind,
    Forward,
    Play,
    Pause,
    Stop,
    Record,
}

fn transport_icon(p: &eframe::egui::Painter, c: Pos2, icon: TransportIcon) {
    use eframe::egui::Shape;
    let color = theme::TEXT;
    // Right-pointing triangle with its left edge at `x`; `dir` = -1.0 mirrors it.
    let tri = |x: f32, dir: f32| {
        Shape::convex_polygon(
            vec![pos2(x, c.y - 6.0), pos2(x + 9.0 * dir, c.y), pos2(x, c.y + 6.0)],
            color,
            Stroke::NONE,
        )
    };
    match icon {
        TransportIcon::Rewind => {
            p.add(tri(c.x, -1.0));
            p.add(tri(c.x + 9.0, -1.0));
        }
        TransportIcon::Forward => {
            p.add(tri(c.x - 9.0, 1.0));
            p.add(tri(c.x, 1.0));
        }
        TransportIcon::Play => {
            p.add(tri(c.x - 4.0, 1.0));
        }
        TransportIcon::Pause => {
            for dx in [-4.0, 4.0] {
                p.rect_filled(Rect::from_center_size(c + vec2(dx, 0.0), vec2(4.0, 12.0)), 1.0, color);
            }
        }
        TransportIcon::Stop => {
            p.rect_filled(Rect::from_center_size(c, vec2(11.0, 11.0)), 1.0, color);
        }
        TransportIcon::Record => {
            p.circle_filled(c, 6.0, theme::RED);
        }
    }
}

pub fn master_section(ui: &mut Ui, o: Pos2, x: f32, buses: &mut [crate::model::Bus], ctx: &Ctx) {
    let p = ui.painter().clone();
    text(&p, o + vec2(x + RIGHT_W / 2.0, 290.0), Align2::CENTER_CENTER, "MASTER SECTION", 15.0, theme::TEXT);
    let line = Stroke::new(1.0, theme::SEPARATOR);
    p.line_segment([o + vec2(x + 8.0, 290.0), o + vec2(x + 90.0, 290.0)], line);
    p.line_segment([o + vec2(x + RIGHT_W - 90.0, 290.0), o + vec2(x + RIGHT_W - 8.0, 290.0)], line);

    let col_w = (RIGHT_W - 12.0) / buses.len().max(1) as f32;
    let mut physical_end = x + 6.0;
    for (i, bus) in buses.iter_mut().enumerate() {
        let cx = x + 6.0 + i as f32 * col_w;
        let w = col_w - 6.0;
        if bus.kind == Kind::Hardware {
            physical_end = cx + col_w;
        }

        let mode = button(ui, r(o, cx + 3.0, 300.0, w, 38.0), Id::new(("mode", &bus.key)), bus.mode.label(), bus.mode != BusMode::Normal, theme::BLUE);
        Popup::menu(&mode.on_hover_text("Bus mode")).show(|ui| {
            for m in BusMode::ALL {
                ui.selectable_value(&mut bus.mode, m, m.label().replace('\n', " "));
            }
        });
        let (label, lit, accent) = match (bus.mono, bus.reverse) {
            (true, _) => ("mono", true, theme::BLUE),
            (false, true) => ("reverse", true, theme::ORANGE),
            (false, false) => ("mono", false, theme::BLUE),
        };
        let resp = button(ui, r(o, cx + 3.0, 344.0, w, 23.0), Id::new(("bmono", &bus.key)), label, lit, accent);
        if resp.on_hover_text("Click to cycle: stereo, mono, reverse (swap left and right)").clicked() {
            (bus.mono, bus.reverse) = match (bus.mono, bus.reverse) {
                (false, false) => (true, false),
                (true, _) => (false, true),
                (false, true) => (false, false),
            };
        }
        if button(ui, r(o, cx + 3.0, 371.0, w, 23.0), Id::new(("beq", &bus.key)), "EQ", bus.eq, theme::BLUE).clicked() {
            bus.eq = !bus.eq;
        }
        if button(ui, r(o, cx + 3.0, 398.0, w, 23.0), Id::new(("bmute", &bus.key)), "Mute", bus.mute, theme::RED).clicked() {
            bus.mute = !bus.mute;
        }

        // Bus name doubles as the device picker (virtual buses bind to VB-Cable on Windows).
        let label_rect = r(o, cx, 426.0, w, 14.0);
        let resp = ui.interact(label_rect, Id::new(("bus-label", &bus.key)), Sense::click());
        let label = ctx.device_label(&bus.key, &bus.device, "");
        let key_color = match (&label.problem, &bus.device) {
            (Some(_), _) => theme::RED,
            (None, Some(_)) => theme::TEXT,
            (None, None) => theme::TEXT_DIM,
        };
        let key_end = text(&p, label_rect.left_center(), Align2::LEFT_CENTER, &bus.key, 11.0, key_color);
        let is_virtual = bus.kind == Kind::Virtual;
        if !(is_virtual && ctx.can_create_virtual) {
            // Small ▼ so it's discoverable that the label opens a device menu.
            let c = pos2(key_end.right() + 6.0, key_end.center().y);
            p.add(eframe::egui::Shape::convex_polygon(
                vec![c + vec2(-3.0, -2.0), c + vec2(3.0, -2.0), c + vec2(0.0, 2.0)],
                key_color,
                Stroke::NONE,
            ));
            let hover = match (label.problem, &bus.device) {
                (Some(problem), _) => format!("{}: {problem}", label.text),
                (None, Some(_)) if is_virtual => format!("Apps record {} from {}  (click to change)", bus.key, label.text),
                (None, Some(_)) => format!("{} plays into {}  (click to change)", bus.key, label.text),
                (None, None) if is_virtual => format!("Choose the cable apps record {} from (e.g. CABLE Output)", bus.key),
                (None, None) => format!("Choose output device for {}", bus.key),
            };
            // Virtual buses are named by the side apps record from; hardware buses by the device played into.
            let direction = if is_virtual { Direction::Capture } else { Direction::Playback };
            let taken = |d: &DeviceInfo| if is_virtual { cable_taken(ctx.strip_cables, d) } else { None };
            device_menu(&resp.on_hover_text(hover), &mut bus.device, candidates(ctx, direction, is_virtual), taken);
        } else {
            let device = crate::model::virtual_bus_device(&bus.key).description;
            resp.on_hover_text(format!("Apps record {} from the \"{device}\" input device", bus.key));
        }

        meter(&p, r(o, cx + 2.0, 444.0, 16.0, 156.0), ctx.peaks(&bus.key));
        fader(ui, r(o, cx + 18.0, 438.0, w - 14.0, 164.0), Id::new(("bfader", &bus.key)), &mut bus.gain_db, bus.mute);
    }

    // PHYSICAL / VIRTUAL captions under the bus groups.
    let caption = |x0: f32, x1: f32, label: &str| {
        if x1 - x0 < 10.0 {
            return;
        }
        let y = 610.0;
        let mid = (x0 + x1) / 2.0;
        let t = text(&p, o + vec2(mid, y), Align2::CENTER_CENTER, label, 9.0, theme::TEXT_FAINT);
        p.line_segment([o + vec2(x0 + 4.0, y), pos2(t.left() - 4.0, t.center().y)], line);
        p.line_segment([pos2(t.right() + 4.0, t.center().y), o + vec2(x1 - 4.0, y)], line);
    };
    caption(x + 6.0, physical_end, "PHYSICAL");
    caption(physical_end, x + RIGHT_W - 6.0, "VIRTUAL");
    vline(ui, o, physical_end, 300.0, 604.0);
}

/// Separators between sections. `strip_edges` are `(x, top)` pairs.
pub fn separators(ui: &Ui, o: Pos2, strip_edges: &[(f32, f32)], right_x: f32) {
    let p = ui.painter();
    p.line_segment([o + vec2(0.0, HEADER_BOTTOM), o + vec2(right_x, HEADER_BOTTOM)], Stroke::new(1.0, theme::SEPARATOR));
    for &(x, top) in strip_edges {
        vline(ui, o, x, top, DESIGN_H - 4.0);
    }
    p.line_segment(
        [o + vec2(right_x, TITLE_H), o + vec2(right_x, DESIGN_H)],
        Stroke::new(3.0, eframe::egui::Color32::from_rgb(30, 42, 54)),
    );
}
