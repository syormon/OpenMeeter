//! Macro Buttons window: a row per macro with its name, hotkey, action and a
//! button to run it.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Align2, RichText, Stroke, Ui};

use super::theme;
use crate::macros::{self, Macro, MacroAction, Targets};
use crate::model::{MAX_GAIN_DB, MIN_GAIN_DB};

/// Window state that isn't saved.
#[derive(Default)]
pub struct State {
    pub open: bool,
    /// The macro whose hotkey is being set by pressing it.
    pub capturing: Option<usize>,
}

/// What the window shows besides the macros themselves.
pub struct Info<'a> {
    /// Strips that actions can target: (key, label).
    pub strips: &'a [(String, String)],
    /// Buses that actions can target: (key, label).
    pub buses: &'a [(String, String)],
    /// The scripts in the scripts folder, by name.
    pub scripts: &'a [String],
    /// Why a macro's hotkey isn't active, by macro index.
    pub errors: &'a HashMap<usize, String>,
    /// Why global hotkeys don't work at all here, if they don't.
    pub unavailable: Option<&'a str>,
}

pub enum Request {
    /// Run macro `n` now.
    Run(usize),
    /// Open the Macro Scripts window, at this script if one is named.
    EditScript(Option<String>),
}

pub fn window(ctx: &egui::Context, state: &mut State, list: &mut Vec<Macro>, info: &Info) -> Option<Request> {
    if let Some(i) = state.capturing {
        capture(ctx, state, list, i);
    }
    let mut request = None;
    let frame = egui::Frame::window(&ctx.global_style()).fill(theme::colors().bg).stroke(Stroke::new(1.0, theme::colors().panel_stroke));
    let mut open = state.open;
    egui::Window::new("Macro Buttons")
        .open(&mut open)
        .frame(frame)
        .resizable(false)
        .collapsible(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .constrain(true)
        .show(ctx, |ui| {
            ui.set_width(940.0);
            if let Some(why) = info.unavailable {
                ui.label(RichText::new(format!("Global hotkeys are unavailable: {why}. Run buttons still work.")).color(theme::colors().orange));
                ui.add_space(4.0);
            }
            ui.label(
                RichText::new("Hotkeys work in any app, even with OpenMeeter hidden. Click a hotkey box, then press the combo (Esc cancels).")
                    .color(theme::colors().text_dim),
            );
            ui.add_space(6.0);
            let mut delete = None;
            egui::Grid::new("macros").num_columns(6).spacing([8.0, 6.0]).show(ui, |ui| {
                for header in ["Name", "Hotkey", "Action", "", "", ""] {
                    ui.label(RichText::new(header).color(theme::colors().text_faint));
                }
                ui.end_row();
                for (i, m) in list.iter_mut().enumerate() {
                    ui.add_sized([150.0, 20.0], egui::TextEdit::singleline(&mut m.name));
                    hotkey_cell(ui, state, m, i);
                    if let Some(script) = action_cell(ui, m, i, info) {
                        request = Some(Request::EditScript(script));
                    }
                    if ui.button("Run").clicked() {
                        request = Some(Request::Run(i));
                    }
                    if ui.button("✖").on_hover_text("Delete this macro").clicked() {
                        delete = Some(i);
                    }
                    match info.errors.get(&i) {
                        Some(e) => ui.label(RichText::new(e).color(theme::colors().red)),
                        None => ui.label(""),
                    };
                    ui.end_row();
                }
            });
            if let Some(i) = delete {
                list.remove(i);
                state.capturing = None;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("+ Add Macro").clicked() {
                    list.push(Macro { name: format!("Macro {}", list.len() + 1), ..Macro::default() });
                }
                if ui.button("Macro Scripts...").on_hover_text("Write Lua scripts for the Run Script action").clicked() {
                    request = Some(Request::EditScript(None));
                }
            });
        });
    if !open {
        state.capturing = None;
    }
    state.open = open;
    request
}

/// Turn the next key press into macro `i`'s hotkey.
fn capture(ctx: &egui::Context, state: &mut State, list: &mut [Macro], i: usize) {
    let press = ctx.input(|input| {
        input.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
            _ => None,
        })
    });
    let Some((key, modifiers)) = press else { return };
    if key == egui::Key::Escape && modifiers.is_none() {
        state.capturing = None;
        return;
    }
    if let (Some(name), Some(m)) = (macros::hotkey_name(modifiers, key, macros::numpad_held(key)), list.get_mut(i)) {
        m.hotkey = Some(name);
        state.capturing = None;
    }
    // Keep the press from also activating a focused widget.
    ctx.input_mut(|input| input.consume_key(modifiers, key));
}

fn hotkey_cell(ui: &mut Ui, state: &mut State, m: &mut Macro, i: usize) {
    ui.horizontal(|ui| {
        let capturing = state.capturing == Some(i);
        let text = if capturing {
            RichText::new("Press a key combo...").color(theme::colors().orange)
        } else {
            match &m.hotkey {
                Some(h) => RichText::new(h).color(theme::colors().green),
                None => RichText::new("(none)").color(theme::colors().text_faint),
            }
        };
        if ui.add_sized([160.0, 20.0], egui::Button::new(text).selected(capturing)).clicked() {
            state.capturing = if capturing { None } else { Some(i) };
        }
        if let Some(h) = m.hotkey.clone() {
            if ui.small_button("Clear").clicked() {
                m.hotkey = None;
            } else if macros::has_numpad_twin(&h) {
                let mut numpad = h.rsplit('+').next().is_some_and(|k| k.starts_with("Numpad"));
                if ui.checkbox(&mut numpad, "Numpad").on_hover_text("Use the keypad version of this key").changed() {
                    m.hotkey = Some(macros::set_numpad(&h, numpad));
                }
            }
        }
    });
}

/// Returns `Some` when the script editor should open (at the named script, if any).
fn action_cell(ui: &mut Ui, m: &mut Macro, i: usize, info: &Info) -> Option<Option<String>> {
    let mut edit = None;
    ui.horizontal(|ui| {
        let combo = egui::ComboBox::from_id_salt(("macro-action", i)).width(190.0).height(600.0).selected_text(m.action.label());
        let combo = combo.show_ui(ui, |ui| {
            for kind in MacroAction::kinds() {
                let Some(kind) = kind else {
                    ui.separator();
                    continue;
                };
                let selected = kind.same_kind(&m.action);
                let item = ui.selectable_label(selected, kind.label());
                let item = if kind.hint().is_empty() { item } else { item.on_hover_text(kind.hint()) };
                if item.clicked() && !selected {
                    m.action = kind.with_target_of(&m.action);
                }
            }
        });
        if !m.action.hint().is_empty() {
            combo.response.on_hover_text(m.action.hint());
        }

        let targets: Vec<(String, String)> = match m.action.targets() {
            Targets::None => Vec::new(),
            Targets::Strips => info.strips.to_vec(),
            Targets::StripsAndBuses => info.strips.iter().chain(info.buses).cloned().collect(),
        };
        if let Some(key) = m.action.key_mut() {
            picker(ui, ("macro-target", i), 140.0, key, &targets);
        }
        match &mut m.action {
            MacroAction::PlayClip { path } => file_button(ui, path, "Sound clip", &[("Audio", &["wav", "mp3", "flac", "ogg", "m4a", "aac", "aif", "aiff"])]),
            MacroAction::LoadSettings { path } => file_button(ui, path, "Settings to load", &[("OpenMeeter settings", &["json"])]),
            MacroAction::ToggleRoute { bus, .. } => picker(ui, ("macro-bus", i), 70.0, bus, info.buses),
            MacroAction::RunScript { name } => {
                let mut scripts: Vec<(String, String)> = info.scripts.iter().map(|s| (s.clone(), s.clone())).collect();
                if !name.is_empty() && !info.scripts.contains(name) {
                    scripts.push((name.clone(), format!("{name} (missing)")));
                }
                picker(ui, ("macro-script", i), 140.0, name, &scripts);
                if ui.button("Edit").on_hover_text("Open it in Macro Scripts").clicked() {
                    edit = Some((!name.is_empty()).then(|| name.clone()));
                }
            }
            MacroAction::SetGain { db, .. } => {
                ui.add(egui::DragValue::new(db).range(MIN_GAIN_DB..=MAX_GAIN_DB).speed(0.1).fixed_decimals(1).suffix(" dB"));
            }
            MacroAction::ChangeGain { db, .. } => {
                let sign = if *db > 0.0 { "+" } else { "" };
                ui.add(egui::DragValue::new(db).range(-24.0..=24.0).speed(0.1).fixed_decimals(1).suffix(" dB").prefix(sign));
            }
            _ => {}
        }
    });
    edit
}

/// A drop-down choosing one of `options` (key, label) into `key`.
fn picker(ui: &mut Ui, id: impl egui::AsIdSalt, width: f32, key: &mut String, options: &[(String, String)]) {
    let current = options.iter().find(|(k, _)| k == key).map_or("Choose...", |(_, label)| label.as_str());
    egui::ComboBox::from_id_salt(id).width(width).selected_text(current).show_ui(ui, |ui| {
        for (k, label) in options {
            ui.selectable_value(key, k.clone(), label);
        }
    });
}

fn file_button(ui: &mut Ui, path: &mut Option<PathBuf>, title: &str, filters: &[(&str, &[&str])]) {
    let name = path.as_ref().and_then(|p| p.file_name()).map_or("Choose file...".to_string(), |n| n.to_string_lossy().into_owned());
    let button = ui.add_sized([170.0, 20.0], egui::Button::new(name).truncate());
    let button = match path {
        Some(p) => button.on_hover_text(p.display().to_string()),
        None => button,
    };
    if button.clicked() {
        let dialog = filters.iter().fold(rfd::FileDialog::new().set_title(title), |d, (name, ext)| d.add_filter(*name, ext));
        if let Some(picked) = dialog.pick_file() {
            *path = Some(picked);
        }
    }
}
