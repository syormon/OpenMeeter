//! Recorder Options, laid out like Voicemeeter's: I/O arming on top, file
//! settings in the middle, playback options at the bottom.

use eframe::egui::{self, Align2, Color32, Popup, RichText, Sense, Stroke, StrokeKind, Ui, vec2};

use super::panels::RecorderAction;
use super::theme;
use crate::model::{MAX_GAIN_DB, MIN_GAIN_DB, RecordFormat, RecordSource, RecorderSettings};
use crate::recorder::recordings_folder;

const ARMED: Color32 = Color32::from_rgb(240, 100, 85);
const ARMED_IDLE: Color32 = Color32::from_rgb(120, 70, 68);
const VALUE_BOX: Color32 = Color32::from_rgb(34, 46, 58);
const BUTTON_SIZE: egui::Vec2 = egui::vec2(92.0, 44.0);
const STOP_AFTER: [Option<u32>; 8] = [None, Some(1), Some(5), Some(10), Some(15), Some(30), Some(60), Some(120)];

/// An armable input strip or bus: node key plus the two lines on its button.
pub struct Armable {
    pub key: String,
    pub kind: &'static str,
    pub name: String,
}

pub fn window(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut RecorderSettings,
    inputs: &[Armable],
    buses: &[Armable],
) -> Option<RecorderAction> {
    let mut action = None;
    let frame = egui::Frame::window(&ctx.global_style()).fill(theme::BG).stroke(Stroke::new(1.0, theme::PANEL_STROKE));
    egui::Window::new("Recorder Options")
        .open(open)
        .frame(frame)
        .resizable(false)
        .collapsible(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .constrain(true)
        .show(ctx, |ui| {
            ui.set_width(760.0);
            heading(ui, "RECORDER OPTIONS : I/O ARMING");
            ui.add_space(6.0);

            let pre = settings.source == RecordSource::PreFaderInputs;
            if arm_row(ui, pre, "PRE-FADER INPUTS", "Arm Inputs To Record", inputs, &mut settings.armed_inputs) {
                settings.source = RecordSource::PreFaderInputs;
            }
            ui.add_space(6.0);
            if arm_row(ui, !pre, "POST-FADER OUTPUTS", "Record BUS Outputs (summed)", buses, &mut settings.record_buses) {
                settings.source = RecordSource::PostFaderOutputs;
            }
            ui.add_space(10.0);

            egui::Frame::new().fill(theme::PANEL).corner_radius(4).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let Some(a) = file_settings(ui, settings) {
                    action = Some(a);
                }
            });
            ui.add_space(8.0);
            playback_settings(ui, settings);
        });
    action
}

fn heading(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).size(17.0).strong().color(theme::TEXT));
        let rect = ui.available_rect_before_wrap();
        let y = rect.center().y;
        ui.painter().line_segment([egui::pos2(rect.left() + 6.0, y), egui::pos2(rect.right(), y)], Stroke::new(1.5, theme::TEXT_DIM));
    });
}

/// One arming row: a mode square, its title, and a button per armable node.
/// Returns true when the row's mode square (or any of its buttons) was clicked.
fn arm_row(
    ui: &mut Ui,
    active: bool,
    title: &str,
    subtitle: &str,
    nodes: &[Armable],
    armed: &mut std::collections::BTreeSet<String>,
) -> bool {
    let mut selected = false;
    ui.horizontal(|ui| {
        let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
        let p = ui.painter();
        p.rect_filled(rect, 2.0, VALUE_BOX);
        p.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::OUTLINE), StrokeKind::Inside);
        if active {
            p.circle_filled(rect.center(), 9.0, Color32::from_rgb(230, 20, 20));
        }
        if resp.on_hover_text("Record this row").clicked() {
            selected = true;
        }

        ui.allocate_ui(vec2(200.0, 40.0), |ui| {
            ui.vertical(|ui| {
                let color = if active { theme::BLUE } else { theme::TEXT_FAINT };
                ui.label(RichText::new(title).size(15.0).strong().color(color));
                ui.label(RichText::new(subtitle).small().color(if active { theme::TEXT } else { theme::TEXT_FAINT }));
            });
        });

        for node in nodes {
            let on = armed.contains(&node.key);
            let (rect, resp) = ui.allocate_exact_size(BUTTON_SIZE, Sense::click());
            let p = ui.painter();
            let radius = 10.0;
            match (on, active) {
                (true, true) => {
                    p.rect_filled(rect, radius, ARMED);
                }
                (true, false) => {
                    p.rect_filled(rect, radius, ARMED_IDLE);
                }
                (false, _) => {
                    let stroke = if resp.hovered() { theme::TEXT_DIM } else { theme::OUTLINE };
                    p.rect_stroke(rect, radius, Stroke::new(1.5, stroke), StrokeKind::Inside);
                }
            }
            let (top, bottom) = match (on, active) {
                (true, true) => (Color32::from_rgb(255, 225, 220), Color32::WHITE),
                (true, false) => (theme::TEXT_DIM, theme::TEXT_DIM),
                (false, _) => (theme::TEXT_FAINT, theme::TEXT_FAINT),
            };
            p.text(rect.center() - vec2(0.0, 9.0), Align2::CENTER_CENTER, node.kind, theme::font(11.0), top);
            let clip = p.with_clip_rect(rect.shrink(4.0));
            clip.text(rect.center() + vec2(0.0, 8.0), Align2::CENTER_CENTER, &node.name, egui::FontId::proportional(15.0), bottom);
            if resp.on_hover_text(format!("Arm {} for recording", node.name)).clicked() {
                if on {
                    armed.remove(&node.key);
                } else {
                    armed.insert(node.key.clone());
                }
                selected = true;
            }
        }
    });
    selected
}

/// Dark box showing a value, like Voicemeeter's option fields.
fn value_box(ui: &mut Ui, text: &str, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
    let fill = if resp.hovered() { Color32::from_rgb(46, 60, 74) } else { VALUE_BOX };
    let p = ui.painter();
    p.rect_filled(rect, 2.0, fill);
    p.text(rect.center(), Align2::CENTER_CENTER, text, egui::FontId::proportional(14.0), Color32::WHITE);
    resp
}

fn key_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::TEXT_DIM));
}

fn yes_no(ui: &mut Ui, value: &mut bool) {
    if value_box(ui, if *value { "Yes" } else { "No" }, 58.0).clicked() {
        *value = !*value;
    }
}

fn file_settings(ui: &mut Ui, settings: &mut RecorderSettings) -> Option<RecorderAction> {
    let mut action = None;
    egui::Grid::new("recorder-files").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
        key_label(ui, "Target Directory:");
        ui.horizontal(|ui| {
            let mut folder = recordings_folder(settings).display().to_string();
            let edit = egui::TextEdit::singleline(&mut folder).desired_width(520.0).text_color(Color32::WHITE);
            if ui.add(edit).changed() {
                settings.folder = Some(folder.into());
            }
            if ui.button("Browse...").clicked() {
                action = Some(RecorderAction::ChooseFolder);
            }
            if ui.button("Open").on_hover_text("Open the folder").clicked() {
                action = Some(RecorderAction::OpenFolder);
            }
        });
        ui.end_row();

        key_label(ui, "Prefix Name:");
        let edit = egui::TextEdit::singleline(&mut settings.prefix).desired_width(520.0).char_limit(40).hint_text("(none: date and time only)");
        ui.add(edit).on_hover_text("Files are named \"<prefix> <date> <time>.wav\"");
        ui.end_row();
    });

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        key_label(ui, "File Type:");
        value_box(ui, "WAVE", 70.0).on_hover_text("Recordings are WAV files");
        ui.add_space(14.0);
        key_label(ui, "Sample Rate:");
        value_box(ui, "48000 Hz", 90.0).on_hover_text("The engine records at 48 kHz");
        ui.add_space(14.0);

        key_label(ui, "Bit Resolution:");
        let bits = match settings.format {
            RecordFormat::Pcm16 => "16 Bits",
            RecordFormat::Pcm24 => "24 Bits",
            RecordFormat::Float32 => "32 Bits Float",
        };
        let resp = value_box(ui, bits, 110.0);
        Popup::menu(&resp).show(|ui| {
            for f in RecordFormat::ALL {
                ui.selectable_value(&mut settings.format, f, f.label());
            }
        });
        ui.add_space(14.0);

        key_label(ui, "Channels:");
        let resp = value_box(ui, &settings.channels.to_string(), 44.0).on_hover_text("1 = mono (left and right mixed), 2 = stereo");
        Popup::menu(&resp).show(|ui| {
            ui.selectable_value(&mut settings.channels, 1, "1 (mono)");
            ui.selectable_value(&mut settings.channels, 2, "2 (stereo)");
        });
    });

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(ui.available_width() - 470.0);
        key_label(ui, "MULTITRACK OPTION > Generates one WAVE file per armed input/bus:");
        yes_no(ui, &mut settings.multitrack);
    });
    action
}

fn playback_settings(ui: &mut Ui, settings: &mut RecorderSettings) {
    egui::Frame::new().fill(Color32::from_rgb(24, 32, 40)).corner_radius(4).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            key_label(ui, "Play On Load:");
            yes_no(ui, &mut settings.play_on_load);
            ui.add_space(12.0);
            key_label(ui, "Loop:");
            yes_no(ui, &mut settings.loop_playback);
            ui.add_space(12.0);

            key_label(ui, "Playback Gain:");
            ui.spacing_mut().slider_width = 140.0;
            let slider = egui::Slider::new(&mut settings.playback_gain_db, MIN_GAIN_DB..=MAX_GAIN_DB).fixed_decimals(1).suffix(" dB");
            if ui.add(slider).on_hover_text("Level of loaded files as they play into the buses (double-click resets)").double_clicked() {
                settings.playback_gain_db = 0.0;
            }
            ui.add_space(12.0);

            key_label(ui, "Stop Record After:");
            let current = settings.stop_after_minutes.map_or("No".to_string(), |m| format!("{m} min"));
            let resp = value_box(ui, &current, 76.0);
            Popup::menu(&resp).show(|ui| {
                for option in STOP_AFTER {
                    let text = option.map_or("No".to_string(), |m| format!("{m} min"));
                    ui.selectable_value(&mut settings.stop_after_minutes, option, text);
                }
            });
        });
    });
}
