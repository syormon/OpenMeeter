//! Macro Buttons window: a row per macro with its name, hotkey, action and a
//! button to run it.

use std::collections::HashMap;

use eframe::egui::{self, Align2, RichText, Stroke, Ui};

use super::theme;
use crate::macros::{self, Macro, MacroAction};

/// Window state that isn't saved.
#[derive(Default)]
pub struct State {
    pub open: bool,
    /// The macro whose hotkey is being set by pressing it.
    pub capturing: Option<usize>,
}

/// What the window shows besides the macros themselves.
pub struct Info<'a> {
    /// Strips and buses a mute can target: (key, label).
    pub targets: &'a [(String, String)],
    /// Why a macro's hotkey isn't active, by macro index.
    pub errors: &'a HashMap<usize, String>,
    /// Why global hotkeys don't work at all here, if they don't.
    pub unavailable: Option<&'a str>,
}

/// Returns the index of a macro whose Run button was clicked.
pub fn window(ctx: &egui::Context, state: &mut State, list: &mut Vec<Macro>, info: &Info) -> Option<usize> {
    if let Some(i) = state.capturing {
        capture(ctx, state, list, i);
    }
    let mut run = None;
    let frame = egui::Frame::window(&ctx.global_style()).fill(theme::BG).stroke(Stroke::new(1.0, theme::PANEL_STROKE));
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
            ui.set_width(820.0);
            if let Some(why) = info.unavailable {
                ui.label(RichText::new(format!("Global hotkeys are unavailable: {why}. Run buttons still work.")).color(theme::ORANGE));
                ui.add_space(4.0);
            }
            ui.label(
                RichText::new("Hotkeys work in any app, even with OpenMeeter hidden. Click a hotkey box, then press the combo (Esc cancels).")
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(6.0);
            let mut delete = None;
            egui::Grid::new("macros").num_columns(6).spacing([8.0, 6.0]).show(ui, |ui| {
                for header in ["Name", "Hotkey", "Action", "", "", ""] {
                    ui.label(RichText::new(header).color(theme::TEXT_FAINT));
                }
                ui.end_row();
                for (i, m) in list.iter_mut().enumerate() {
                    ui.add_sized([150.0, 20.0], egui::TextEdit::singleline(&mut m.name));
                    hotkey_cell(ui, state, m, i);
                    action_cell(ui, m, i, info.targets);
                    if ui.button("Run").clicked() {
                        run = Some(i);
                    }
                    if ui.button("✖").on_hover_text("Delete this macro").clicked() {
                        delete = Some(i);
                    }
                    match info.errors.get(&i) {
                        Some(e) => ui.label(RichText::new(e).color(theme::RED)),
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
            if ui.button("+ Add Macro").clicked() {
                list.push(Macro { name: format!("Macro {}", list.len() + 1), ..Macro::default() });
            }
        });
    if !open {
        state.capturing = None;
    }
    state.open = open;
    run
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
            RichText::new("Press a key combo...").color(theme::ORANGE)
        } else {
            match &m.hotkey {
                Some(h) => RichText::new(h).color(theme::GREEN),
                None => RichText::new("(none)").color(theme::TEXT_FAINT),
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

fn action_cell(ui: &mut Ui, m: &mut Macro, i: usize, targets: &[(String, String)]) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("macro-action", i)).width(170.0).selected_text(m.action.label()).show_ui(ui, |ui| {
            for kind in MacroAction::kinds() {
                let selected = kind.same_kind(&m.action);
                if ui.selectable_label(selected, kind.label()).clicked() && !selected {
                    m.action = kind;
                }
            }
        });
        match &mut m.action {
            MacroAction::PlayClip { path } => {
                let name = path.as_ref().and_then(|p| p.file_name()).map_or("Choose file...".to_string(), |n| n.to_string_lossy().into_owned());
                let button = ui.add_sized([170.0, 20.0], egui::Button::new(name).truncate());
                let button = match path {
                    Some(p) => button.on_hover_text(p.display().to_string()),
                    None => button,
                };
                if button.clicked()
                    && let Some(picked) = rfd::FileDialog::new()
                        .set_title("Sound clip")
                        .add_filter("Audio", &["wav", "mp3", "flac", "ogg", "m4a", "aac", "aif", "aiff"])
                        .pick_file()
                {
                    *path = Some(picked);
                }
            }
            MacroAction::ToggleMute { key } => {
                let current = targets.iter().find(|(k, _)| k == key).map_or("Choose...", |(_, label)| label.as_str());
                egui::ComboBox::from_id_salt(("macro-target", i)).width(160.0).selected_text(current).show_ui(ui, |ui| {
                    for (k, label) in targets {
                        ui.selectable_value(key, k.clone(), label);
                    }
                });
            }
            _ => {}
        }
    });
}
