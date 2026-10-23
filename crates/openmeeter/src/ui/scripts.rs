//! Macro Scripts window: the Lua scripts in the scripts folder, an editor, what
//! they print, and the API reference.

use std::collections::VecDeque;
use std::path::Path;

use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{self, Align2, RichText, Stroke};
use egui_extras::syntax_highlighting::{self, CodeTheme};

use super::theme;
use crate::model::Mixer;
use crate::script::complete::{self, Item, Kind, Names};
use crate::script::params::{self, Host, Lookup, Value};
use crate::script::{self, OutputLine};

#[derive(Default)]
pub struct State {
    pub open: bool,
    names: Vec<String>,
    /// The script in the editor, its text, and the text as last saved.
    selected: Option<String>,
    text: String,
    saved: String,
    new_name: String,
    /// The last file error, shown under the buttons.
    problem: Option<String>,
    /// The completion list, while it's open: the highlighted item, how many there
    /// are, and where the word being completed starts.
    popup: Option<usize>,
    popup_len: usize,
    popup_start: usize,
}

pub enum Action {
    Run(String),
    OpenFolder,
    ClearOutput,
}

impl State {
    /// Open the window, showing script `name` if it's given and exists.
    pub fn show(&mut self, folder: &Path, name: Option<&str>) {
        self.open = true;
        self.rescan(folder);
        if let Some(name) = name.filter(|n| self.names.iter().any(|m| m == n)) {
            self.select(folder, name);
        } else if self.selected.is_none()
            && let Some(first) = self.names.first().cloned()
        {
            self.select(folder, &first);
        }
    }

    fn rescan(&mut self, folder: &Path) {
        self.names = script::list(folder);
    }

    fn select(&mut self, folder: &Path, name: &str) {
        self.save(folder);
        match std::fs::read_to_string(path(folder, name)) {
            Ok(text) => {
                self.selected = Some(name.to_string());
                self.saved = text.clone();
                self.text = text;
                self.problem = None;
            }
            Err(e) => self.problem = Some(format!("Couldn't open {name}.lua: {e}")),
        }
    }

    /// Save the open script if it changed.
    fn save(&mut self, folder: &Path) {
        let Some(name) = &self.selected else { return };
        if self.text == self.saved {
            return;
        }
        let result = std::fs::create_dir_all(folder).and_then(|()| std::fs::write(path(folder, name), &self.text));
        match result {
            Ok(()) => {
                self.saved = self.text.clone();
                self.problem = None;
            }
            Err(e) => self.problem = Some(format!("Couldn't save {name}.lua: {e}")),
        }
    }

    fn create(&mut self, folder: &Path) {
        let name = self.new_name.trim().to_string();
        if !script::valid_name(&name) {
            self.problem = Some("Script names can have letters, digits, spaces, - and _".into());
            return;
        }
        let file = path(folder, &name);
        if !file.exists() {
            let result = std::fs::create_dir_all(folder).and_then(|()| std::fs::write(&file, script::template(&name)));
            if let Err(e) = result {
                self.problem = Some(format!("Couldn't create {name}.lua: {e}"));
                return;
            }
        }
        self.new_name.clear();
        self.rescan(folder);
        self.select(folder, &name);
    }
}

fn path(folder: &Path, name: &str) -> std::path::PathBuf {
    folder.join(format!("{name}.lua"))
}

/// `mixer` and `buttons` (macro names) feed the completions and the help line.
pub fn window(
    ctx: &egui::Context,
    state: &mut State,
    folder: &Path,
    output: &VecDeque<OutputLine>,
    mixer: &Mixer,
    buttons: &[String],
) -> Option<Action> {
    if !state.open {
        return None;
    }
    let mut action = None;
    let colors = theme::colors();
    let frame = egui::Frame::window(&ctx.global_style()).fill(colors.bg).stroke(Stroke::new(1.0, colors.panel_stroke));
    let mut open = state.open;
    egui::Window::new("Macro Scripts")
        .open(&mut open)
        .frame(frame)
        .resizable(false)
        .collapsible(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .constrain(true)
        // Scrolls rather than running off a small main window.
        .vscroll(true)
        .max_height(ctx.content_rect().height() - 50.0)
        .show(ctx, |ui| {
            ui.set_width(900.0);
            ui.label(
                RichText::new("Lua 5.3 scripts using Voicemeeter's parameter names. Bind one to a hotkey with the Run Script action in Macro Buttons.")
                    .color(colors.text_dim),
            );
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    egui::ScrollArea::vertical().id_salt("script-list").max_height(170.0).show(ui, |ui| {
                        if state.names.is_empty() {
                            ui.label(RichText::new("No scripts yet").color(colors.text_faint));
                        }
                        for name in state.names.clone() {
                            let selected = state.selected.as_deref() == Some(name.as_str());
                            if ui.selectable_label(selected, &name).clicked() && !selected {
                                state.select(folder, &name);
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let field = ui.add(egui::TextEdit::singleline(&mut state.new_name).hint_text("new script").desired_width(110.0));
                        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("New").clicked() || entered {
                            state.create(folder);
                        }
                    });
                    if ui.button("Open Scripts Folder").clicked() {
                        action = Some(Action::OpenFolder);
                    }
                    if ui.button("Refresh").on_hover_text("Pick up scripts added or edited outside OpenMeeter").clicked() {
                        state.rescan(folder);
                        if let Some(name) = state.selected.clone()
                            && state.text == state.saved
                        {
                            state.select(folder, &name);
                        }
                    }
                });
                // Not ui.separator(): in a row it would stretch to the window's full height.
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    let Some(name) = state.selected.clone() else {
                        ui.label(RichText::new("Create a script, or pick one on the left.").color(colors.text_dim));
                        ui.label(RichText::new(format!("Scripts are kept in {}", folder.display())).color(colors.text_faint));
                        return;
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{name}.lua")).color(colors.text_bright).strong());
                        let unsaved = state.text != state.saved;
                        if ui.add_enabled(unsaved, egui::Button::new("Save")).on_hover_text("Ctrl+S").clicked() {
                            state.save(folder);
                        }
                        if ui.add_enabled(unsaved, egui::Button::new("Revert")).clicked() {
                            state.text = state.saved.clone();
                        }
                        if ui.button("▶ Run").on_hover_text("Save, then run it now").clicked() {
                            state.save(folder);
                            action = Some(Action::Run(name.clone()));
                        }
                        if unsaved {
                            ui.label(RichText::new("unsaved").color(colors.orange));
                        }
                    });
                    if let Some(problem) = &state.problem {
                        ui.label(RichText::new(problem).color(colors.red));
                    }
                    let names = Names {
                        strips: mixer.strips.iter().map(|s| s.label.clone()).collect(),
                        buses: mixer.buses.iter().map(|b| b.key.clone()).collect(),
                        buttons: buttons.to_vec(),
                    };
                    let editor_id = egui::Id::new("script-editor");
                    let focused = ui.memory(|m| m.has_focus(editor_id));

                    // An open list takes the arrow keys, Enter, Tab and Esc before the editor sees them.
                    let mut accept = false;
                    if focused && let Some(selected) = &mut state.popup {
                        let n = state.popup_len.max(1);
                        let key = |k| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
                        if key(egui::Key::ArrowDown) {
                            *selected = (*selected + 1) % n;
                        }
                        if key(egui::Key::ArrowUp) {
                            *selected = (*selected + n - 1) % n;
                        }
                        accept = key(egui::Key::Enter) || key(egui::Key::Tab);
                        if key(egui::Key::Escape) {
                            state.popup = None;
                        }
                    }
                    let manual = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Space));
                    static HOOK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false); let manual = manual || (std::env::var("OM_SHOT").is_ok() && { if !HOOK.swap(true, std::sync::atomic::Ordering::Relaxed) { let at = state.text.find("mic.").map_or(0, |i| i + 6); let c = state.text[..at].chars().count(); let mut st = egui::TextEdit::load_state(ui.ctx(), editor_id).unwrap_or_default(); st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(c)))); egui::TextEdit::store_state(ui.ctx(), editor_id, st); ui.ctx().memory_mut(|m| m.request_focus(editor_id)); } state.popup.is_none() }); // TEMP-SHOT
                    let mut reopen = false;
                    if accept
                        && let Some(selected) = state.popup
                        && let Some(cursor) = editor_cursor(ui.ctx(), editor_id, &state.text)
                        && let Some(completion) = complete::complete(&state.text, cursor, &names)
                        && let Some(item) = completion.items.get(selected)
                    {
                        reopen = insert_completion(ui.ctx(), editor_id, &mut state.text, completion.start, cursor, &item.insert);
                    }

                    let code_theme = CodeTheme::from_style(ui.style());
                    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                        // Memoized by egui_extras, so this is cheap every frame.
                        let mut job = syntax_highlighting::highlight(ui.ctx(), ui.style(), &code_theme, text.as_str(), "lua");
                        job.wrap.max_width = wrap_width;
                        ui.fonts_mut(|fonts| fonts.layout_job(job))
                    };
                    let output = egui::ScrollArea::vertical()
                        .id_salt("script-text")
                        .max_height(230.0)
                        .show(ui, |ui| {
                            egui::TextEdit::multiline(&mut state.text)
                                .id(editor_id)
                                .code_editor()
                                .desired_rows(13)
                                .desired_width(f32::INFINITY)
                                .lock_focus(true)
                                .layouter(&mut layouter)
                                .show(ui)
                        })
                        .inner;
                    let focused = output.response.has_focus() || std::env::var("OM_SHOT").is_ok(); // TEMP-SHOT
                    if focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
                        state.save(folder);
                    }

                    // The cursor, when there's no selection.
                    let cursor = output.cursor_range.filter(|r| r.primary.index == r.secondary.index).map(|r| r.primary.index.0);
                    let cursor = cursor.or_else(|| std::env::var("OM_SHOT").ok().and_then(|_| state.text.find("mic.").map(|i| state.text[..i + 6].chars().count()))); // TEMP-SHOT
                    let cursor_byte = cursor.map(|c| byte_index(&state.text, c));
                    let completion = cursor_byte.filter(|_| focused).and_then(|c| complete::complete(&state.text, c, &names));
                    let typed = output.response.changed();
                    match &completion {
                        Some(c) if typed || reopen || manual || state.popup.is_some() => {
                            // Moving the cursor to another word closes the list; typing keeps it.
                            if state.popup.is_some() && !typed && !reopen && !manual && c.start != state.popup_start {
                                state.popup = None;
                            } else {
                                let selected = if typed || reopen || manual { 0 } else { state.popup.unwrap_or(0) };
                                state.popup = Some(selected.min(c.items.len() - 1));
                                state.popup_len = c.items.len();
                                state.popup_start = c.start;
                            }
                        }
                        _ => state.popup = None,
                    }

                    if let (Some(selected), Some(completion), Some(cursor)) = (state.popup, &completion, cursor) {
                        let caret = output.galley.pos_from_cursor(CCursor::new(cursor)).translate(output.galley_pos.to_vec2());
                        let clicked = completion_list(ui.ctx(), caret.left_bottom() + egui::vec2(0.0, 2.0), &completion.items, selected);
                        if let (Some(i), Some(cursor_byte)) = (clicked, cursor_byte) {
                            let item = &completion.items[i];
                            if !insert_completion(ui.ctx(), editor_id, &mut state.text, completion.start, cursor_byte, &item.insert) {
                                state.popup = None;
                            }
                            ui.ctx().memory_mut(|m| m.request_focus(editor_id));
                        }
                    }

                    // What the parameter under the cursor is, and its value now.
                    let help = cursor_byte.and_then(|c| complete::describe(&state.text, c, &names));
                    let help = match help {
                        Some((path, detail)) => {
                            let mut host = Host::new(mixer.clone(), false, Default::default(), buttons.len());
                            let now = match params::get(&mut host, &path) {
                                Ok(Lookup::Value(Value::Int(i))) => format!("  (now {i})"),
                                Ok(Lookup::Value(Value::Num(n))) => format!("  (now {n})"),
                                Ok(Lookup::Value(Value::Str(s))) if !s.is_empty() => format!("  (now \"{s}\")"),
                                _ => String::new(),
                            };
                            RichText::new(format!("{path}: {detail}{now}")).color(colors.text_dim)
                        }
                        None => RichText::new("Ctrl+Space lists what can go here").color(colors.text_faint),
                    };
                    ui.label(help.monospace().size(11.5));
                });
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Output").color(colors.text_faint));
                if ui.small_button("Clear").clicked() {
                    action = Some(Action::ClearOutput);
                }
            });
            egui::Frame::NONE.fill(colors.inset).inner_margin(4.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("script-output").max_height(80.0).min_scrolled_height(80.0).stick_to_bottom(true).show(
                    ui,
                    |ui| {
                        ui.set_width(ui.available_width());
                        for line in output {
                            let color = if line.error { colors.red } else { colors.text };
                            ui.label(RichText::new(&line.text).monospace().color(color));
                        }
                    },
                );
            });
            ui.add_space(4.0);
            egui::CollapsingHeader::new("API reference").show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("script-reference").max_height(260.0).show(ui, |ui| {
                    ui.label(RichText::new(script::REFERENCE).monospace().size(11.5).color(colors.text));
                });
            });
        });
    if !open {
        state.save(folder);
    }
    state.open = open;
    action
}

/// Byte offset of character `index` in `text`.
fn byte_index(text: &str, index: usize) -> usize {
    text.char_indices().nth(index).map_or(text.len(), |(byte, _)| byte)
}

/// The editor's cursor as a byte offset, if nothing is selected.
fn editor_cursor(ctx: &egui::Context, id: egui::Id, text: &str) -> Option<usize> {
    let range = egui::TextEdit::load_state(ctx, id)?.cursor.char_range()?;
    (range.primary.index == range.secondary.index).then(|| byte_index(text, range.primary.index.0))
}

/// Replace `start..cursor` with `insert` and put the cursor after it. Returns
/// whether the list should stay open (after a group, for what's inside).
fn insert_completion(ctx: &egui::Context, id: egui::Id, text: &mut String, start: usize, cursor: usize, insert: &str) -> bool {
    text.replace_range(start..cursor, insert);
    let after = text[..start + insert.len()].chars().count();
    let mut editor = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    editor.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(after))));
    egui::TextEdit::store_state(ctx, id, editor);
    insert.ends_with(['.', '['])
}

/// The completion list under the cursor, with the highlighted item's details.
/// Returns the item clicked, if any.
fn completion_list(ctx: &egui::Context, at: egui::Pos2, items: &[Item], selected: usize) -> Option<usize> {
    const ROWS: usize = 8;
    let colors = theme::colors();
    let first = selected.saturating_sub(ROWS - 1);
    let mut clicked = None;
    egui::Area::new(egui::Id::new("script-completions")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).fill(colors.bg).show(ui, |ui| {
            ui.set_width(360.0);
            for (i, item) in items.iter().enumerate().skip(first).take(ROWS) {
                let kind = match item.kind {
                    Kind::Parameter => "param",
                    Kind::Group => "group",
                    Kind::Index => item.detail.as_str(),
                    Kind::Function => "function",
                    Kind::Keyword => "keyword",
                    Kind::Variable => "local",
                };
                let row = ui.horizontal(|ui| {
                    let label = ui.selectable_label(i == selected, RichText::new(&item.label).monospace());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(kind).size(11.0).color(colors.text_faint));
                    });
                    label
                });
                if row.inner.clicked() {
                    clicked = Some(i);
                }
            }
            if items.len() > ROWS {
                ui.label(RichText::new(format!("{} more: keep typing or use the arrows", items.len() - ROWS)).size(11.0).color(colors.text_faint));
            }
            if let Some(item) = items.get(selected).filter(|i| i.kind != Kind::Index) {
                ui.separator();
                ui.label(RichText::new(&item.detail).size(11.5).color(colors.text_dim));
            }
        });
    });
    clicked
}
