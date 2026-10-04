//! The Menu button (modelled on Voicemeeter's) and the windows it opens.

use std::collections::HashMap;

use eframe::egui::{self, RichText, Ui};
use openmeeter_backend::{DeviceId, DeviceInfo, StreamInfo};

use super::theme;
use crate::config::{AppSettings, AutoRestart};
use crate::model::{Kind, MAX_BUS_DELAY_MS, Mixer};

pub enum MenuAction {
    RestartEngine,
    SetAutoRestart(AutoRestart),
    EjectCassette,
    LoadSettings,
    ChooseStartupPreset,
    ClearStartupPreset,
    SaveSettings,
    ResetSettings,
    SetTray(bool),
    SetRunOnStartup(bool),
    SetShowOnStartup(bool),
    SetAlwaysOnTop(bool),
    SetLockUi(bool),
    OpenSystemSettings,
    OpenRecorderOptions,
    OpenMacros,
    RefreshDevices,
    ShutDown,
}

/// Facts the menu shows that don't live in the settings.
pub struct MenuState<'a> {
    pub settings: &'a AppSettings,
    pub run_on_startup: bool,
    pub tray_available: bool,
    pub cassette_loaded: bool,
}

/// Voicemeeter-style check item: a checkbox that reports the new state.
fn check(ui: &mut Ui, on: bool, label: &str) -> Option<bool> {
    let mut value = on;
    ui.checkbox(&mut value, label).changed().then_some(value)
}

pub fn contents(ui: &mut Ui, state: &MenuState) -> Option<MenuAction> {
    use MenuAction as A;
    let s = state.settings;
    let mut action = None;
    let mut pick = |a| action = Some(a);
    ui.set_min_width(290.0);

    if ui.button("Restart Audio Engine").clicked() {
        pick(A::RestartEngine);
    }
    if let Some(on) = check(ui, s.auto_restart == AutoRestart::A1, "Auto Restart Audio Engine (A1 Device)") {
        pick(A::SetAutoRestart(if on { AutoRestart::A1 } else { AutoRestart::Off }));
    }
    if let Some(on) = check(ui, s.auto_restart == AutoRestart::AllDevices, "Auto Restart Audio Engine (All Devices)") {
        pick(A::SetAutoRestart(if on { AutoRestart::AllDevices } else { AutoRestart::Off }));
    }
    if ui.add_enabled(state.cassette_loaded, egui::Button::new("Eject Cassette (release audio file)")).clicked() {
        pick(A::EjectCassette);
    }
    ui.separator();

    if ui.button("Load Settings...").clicked() {
        pick(A::LoadSettings);
    }
    ui.menu_button("Load Settings on Startup", |ui| {
        let current = s.startup_preset.as_ref().map_or("(none: restore the last session)".to_string(), |p| p.display().to_string());
        ui.label(RichText::new(current).small().weak());
        if ui.button("Choose file...").clicked() {
            pick(A::ChooseStartupPreset);
        }
        if ui.add_enabled(s.startup_preset.is_some(), egui::Button::new("Don't load a file")).clicked() {
            pick(A::ClearStartupPreset);
        }
    });
    if let Some(p) = &s.startup_preset {
        let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
        ui.label(RichText::new(format!("    {name}")).weak());
    }
    if ui.button("Save Settings...").clicked() {
        pick(A::SaveSettings);
    }
    ui.separator();

    if ui.button("Reset Settings (Re-Initialization)...").clicked() {
        pick(A::ResetSettings);
    }
    ui.separator();

    let tray = ui.add_enabled_ui(state.tray_available, |ui| check(ui, s.tray && state.tray_available, "System Tray")).inner;
    if let Some(on) = tray {
        pick(A::SetTray(on));
    }
    let startup_label = if cfg!(windows) { "Run on Windows Startup" } else { "Run on Login" };
    if let Some(on) = check(ui, state.run_on_startup, startup_label) {
        pick(A::SetRunOnStartup(on));
    }
    if let Some(on) = check(ui, s.show_on_startup, "Show App On Startup") {
        pick(A::SetShowOnStartup(on));
    }
    if let Some(on) = check(ui, s.always_on_top, "Set as Always Visible") {
        pick(A::SetAlwaysOnTop(on));
    }
    if let Some(on) = check(ui, s.lock_ui, "Lock Graphic User Interface") {
        pick(A::SetLockUi(on));
    }
    ui.separator();

    if ui.button("System Settings / Options...").clicked() {
        pick(A::OpenSystemSettings);
    }
    if ui.button("Tape Recorder Options...").clicked() {
        pick(A::OpenRecorderOptions);
    }
    if ui.button("Macro Buttons / Hotkeys...").clicked() {
        pick(A::OpenMacros);
    }
    if ui.button("Refresh Device List").clicked() {
        pick(A::RefreshDevices);
    }
    ui.separator();

    if ui.button("Shut Down OpenMeeter").clicked() {
        pick(A::ShutDown);
    }
    action
}

/// Which extra windows are open.
#[derive(Default)]
pub struct Windows {
    pub system_settings: bool,
    pub recorder_options: bool,
    pub confirm_reset: bool,
}

/// What System Settings shows about the running engine.
pub struct SystemInfo<'a> {
    pub devices: &'a [DeviceInfo],
    pub streams: &'a HashMap<String, StreamInfo>,
    pub errors: &'a HashMap<String, String>,
    pub config_path: &'a str,
    /// The backend can open outputs exclusively (Windows).
    pub exclusive_mode: bool,
}

const DEFAULT_BUFFER_MS: u32 = 20;
/// The engine runs at 48 kHz, so 48 samples per millisecond.
const SAMPLES_PER_MS: u32 = 48;

/// System Settings / Options, laid out like Voicemeeter's: a status row per
/// physical input and output, then buffering and per-output delay. Returns true
/// when the buffer size was committed (on release, so dragging doesn't restart
/// audio over and over).
pub fn system_settings(ctx: &egui::Context, open: &mut bool, settings: &mut AppSettings, mixer: &mut Mixer, info: &SystemInfo) -> bool {
    let mut buffer_committed = false;
    let frame = egui::Frame::window(&ctx.global_style()).fill(theme::BG).stroke(egui::Stroke::new(1.0, theme::PANEL_STROKE));
    egui::Window::new("System Settings / Options")
        .open(open)
        .frame(frame)
        .resizable(false)
        .collapsible(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .constrain(true)
        .show(ctx, |ui| {
            ui.set_width(540.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("System Settings / Information").color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("OpenMeeter Version: {}", env!("CARGO_PKG_VERSION"))).color(theme::TEXT_DIM));
                });
            });

            egui::Frame::new().fill(theme::PANEL).corner_radius(8).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut n = 0;
                for strip in mixer.strips.iter().filter(|s| s.kind == Kind::Hardware) {
                    n += 1;
                    let title = format!("IN{n} {}", strip.label);
                    device_row(ui, &title, &strip.key, &strip.device, info, false);
                }
                ui.add_space(6.0);
                for (i, bus) in mixer.buses.iter().filter(|b| b.kind == Kind::Hardware).enumerate() {
                    // Voicemeeter calls A1 the main device: it's the one people listen on.
                    let title = if i == 0 { format!("OUT {} Main Device", bus.key) } else { format!("OUT {}", bus.key) };
                    device_row(ui, &title, &bus.key, &bus.device, info, i == 0);
                }
            });

            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui(egui::vec2(340.0, 90.0), |ui| {
                    ui.set_min_width(340.0);
                    egui::Grid::new("buffering").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label(RichText::new("Buffering WASAPI:").color(theme::TEXT_DIM));
                        let mut samples = settings.buffer_ms * SAMPLES_PER_MS;
                        let resp = ui.add(
                            egui::DragValue::new(&mut samples)
                                .range(10 * SAMPLES_PER_MS..=100 * SAMPLES_PER_MS)
                                .speed(8.0)
                                .custom_formatter(|v, _| format!("{v:.0}")),
                        );
                        if resp.changed() {
                            settings.buffer_ms = (samples + SAMPLES_PER_MS / 2) / SAMPLES_PER_MS;
                        }
                        buffer_committed = resp.drag_stopped() || (resp.changed() && !resp.dragged());
                        let hover = format!(
                            "Audio buffered per input, in samples at 48 kHz ({} ms). Lower = less delay; raise it if you hear crackles. Changing it briefly restarts audio.",
                            settings.buffer_ms
                        );
                        resp.on_hover_text(hover);
                        ui.label(RichText::new(format!("(default: {})", DEFAULT_BUFFER_MS * SAMPLES_PER_MS)).small().color(theme::TEXT_FAINT));
                        ui.end_row();

                        ui.label(RichText::new("Preferred Main SampleRate:").color(theme::TEXT_DIM));
                        ui.add_enabled(false, egui::Button::new("48000 Hz")).on_disabled_hover_text("The engine always runs at 48 kHz; devices are converted.");
                        ui.label("");
                        ui.end_row();
                    });
                });

                ui.vertical(|ui| {
                    ui.label(RichText::new("Monitoring Synchro Delay:").color(theme::TEXT_DIM));
                    egui::Grid::new("delays").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                        for bus in mixer.buses.iter_mut().filter(|b| b.kind == Kind::Hardware) {
                            ui.label(RichText::new(format!("OUT {}:", bus.key)).color(theme::TEXT_DIM));
                            let drag = egui::DragValue::new(&mut bus.delay_ms).range(0.0..=MAX_BUS_DELAY_MS).speed(0.5).fixed_decimals(2).suffix(" ms");
                            ui.add(drag).on_hover_text("Delays this output, e.g. to line your speakers up with a stream or video.");
                            if info.exclusive_mode {
                                ui.checkbox(&mut bus.exclusive, "Exclusive").on_hover_text(
                                    "Open this output in exclusive mode, like Voicemeeter: audio skips the Windows mixer, volume and sound effects, with lower latency. Other apps can't play to the device meanwhile. Changing it briefly restarts audio.",
                                );
                            }
                            ui.end_row();
                        }
                    });
                });
            });

            ui.add_space(6.0);
            ui.label(RichText::new(format!("Config: {}", info.config_path)).small().color(theme::TEXT_FAINT));
        });
    buffer_committed
}

/// One status row: name, stream state and format, then the device underneath.
fn device_row(ui: &mut Ui, title: &str, key: &str, device: &Option<DeviceId>, info: &SystemInfo, highlight: bool) {
    let device_name = device.as_ref().map(|id| info.devices.iter().find(|d| &d.id == id).map_or("(missing device)", |d| d.name.as_str()));
    let stream = info.streams.get(key);
    let error = info.errors.get(key);
    let (status, status_color) = match (device_name, stream, error) {
        (Some(_), _, Some(_)) => ("ERROR", theme::RED),
        (Some(_), Some(_), None) => ("ON", egui::Color32::WHITE),
        _ => ("OFF", theme::TEXT_FAINT),
    };
    let value = if stream.is_some() { egui::Color32::WHITE } else { theme::TEXT_FAINT };

    let mut frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 3)).corner_radius(6);
    if highlight {
        frame = frame.stroke(egui::Stroke::new(1.0, theme::OUTLINE)).fill(theme::BG);
    }
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            cell(ui, 175.0, RichText::new(title).size(14.0).color(theme::TEXT_DIM));
            let resp = key_value(ui, 95.0, "Status: ", status, status_color);
            if let Some(error) = error {
                resp.on_hover_text(error);
            }
            let fmt = |v: Option<String>| v.unwrap_or_else(|| "-".into());
            key_value(ui, 110.0, "SR: ", &fmt(stream.map(|s| format!("{} Hz", s.sample_rate))), value);
            key_value(ui, 70.0, "buf:", &fmt(stream.map(|s| s.buffer_frames.to_string())), value);
            key_value(ui, 40.0, "ch:", &fmt(stream.map(|s| s.channels.to_string())), value);
            key_value(ui, 40.0, "r:", &fmt(stream.map(|s| s.bits.to_string())), value);
        });
        let line = match device_name {
            Some(name) if stream.is_some_and(|s| s.exclusive) => RichText::new(format!("WASAPI exclusive: {name}")).color(theme::TEXT),
            Some(name) => RichText::new(format!("WASAPI: {name}")).color(theme::TEXT),
            None => RichText::new("- none -").color(theme::TEXT_FAINT),
        };
        ui.label(line.small());
    });
}

/// "SR: 48000 Hz" with the key dimmer than the value, as in Voicemeeter.
fn key_value(ui: &mut Ui, width: f32, key: &str, value: &str, value_color: egui::Color32) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::proportional(14.0);
    job.append(key, 0.0, egui::TextFormat::simple(font.clone(), theme::TEXT_FAINT));
    job.append(value, 0.0, egui::TextFormat::simple(font, value_color));
    fixed_width(ui, width, |ui| ui.label(job))
}

/// Lay `add` out in a cell exactly `width` wide, so columns line up across rows.
fn fixed_width(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> egui::Response) -> egui::Response {
    ui.allocate_ui_with_layout(egui::vec2(width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_width(width);
        add(ui)
    })
    .inner
}

fn cell(ui: &mut Ui, width: f32, text: RichText) -> egui::Response {
    fixed_width(ui, width, |ui| ui.label(text))
}

/// Confirmation for Reset Settings. Returns Some(true) to reset, Some(false) to cancel.
pub fn confirm_reset(ctx: &egui::Context) -> Option<bool> {
    let modal = egui::Modal::new(egui::Id::new("confirm-reset")).show(ctx, |ui| {
        ui.set_width(320.0);
        ui.heading("Reset settings?");
        ui.label("All strips, buses, routing, device choices and recorder options go back to their defaults. App options (tray, startup) are kept.");
        ui.add_space(8.0);
        let mut answer = None;
        ui.horizontal(|ui| {
            if ui.button("Reset").clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
        answer
    });
    if modal.backdrop_response.clicked() { Some(false) } else { modal.inner }
}
