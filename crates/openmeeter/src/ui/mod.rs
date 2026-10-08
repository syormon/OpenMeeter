//! Voicemeeter-style skin: a fixed design grid, scaled to fit the window.

mod ballistics;
mod macros;
mod menu;
mod panels;
mod recorder_options;
pub(crate) mod theme;
mod tray;
mod widgets;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Id, Popup, Pos2, Sense, Ui, vec2};
use openmeeter_backend::{AudioBackend, DeviceId, DeviceInfo, Direction, cable_partner, driver_name};

use crate::config::{self, AutoRestart, Config};
use crate::recorder::{self, Recorder};
use panels::RecorderAction;
use crate::model::{Kind, Mixer};
use panels::{Ctx, DESIGN_H, HW_STRIP_W, RIGHT_W, TITLE_H, VIRT_STRIP_W};
use widgets::text;

const METER_FPS: u64 = 60;

/// App icon (1024x1024); also embedded as the .exe icon by build.rs.
const ICON_PNG: &[u8] = include_bytes!("../../../../images/icon.png");
/// The wordmark's bounding box inside the icon, in icon pixels.
const LOGO_CROP: ([usize; 2], [usize; 2]) = ([96, 240], [852, 540]);
const LOGO_HEIGHT: f32 = 28.0;
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// How often the engine's counters are checked for the log, and how often they
/// are logged even when nothing changed.
const STATS_CHECK: Duration = Duration::from_secs(10);
const STATS_HEARTBEAT: Duration = Duration::from_secs(600);
/// Auto-restart: how often to look for recovered devices, backing off to the max
/// while a device keeps failing (e.g. held in exclusive mode by another app).
const RESTART_CHECK_MIN: Duration = Duration::from_secs(2);
const RESTART_CHECK_MAX: Duration = Duration::from_secs(30);

pub fn run(backend: Box<dyn AudioBackend>, config_path: PathBuf) -> anyhow::Result<()> {
    let mut config = config::load(&config_path);
    if let Some(preset) = &config.settings.startup_preset {
        match config::load_preset(preset) {
            Ok(preset) => {
                config.mixer = preset.mixer;
                config.settings.theme = preset.theme;
            }
            Err(e) => log::warn!("startup preset not loaded: {e:#}"),
        }
    }
    let size = design_size(&config.mixer);
    // The app ID ties the window to its desktop entry, which is where docks get the icon.
    let mut viewport = egui::ViewportBuilder::default().with_title("OpenMeeter").with_app_id("openmeeter");
    #[cfg(target_os = "linux")]
    crate::desktop_entry::install(ICON_PNG);
    if config.settings.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    let use_x11 = prefer_x11(config.settings.tray);
    let can_hide = can_hide_window(use_x11);
    // Starting hidden only makes sense with a tray icon to bring the window back.
    let start_in_tray = !config.settings.show_on_startup && config.settings.tray && can_hide;
    if start_in_tray {
        viewport = viewport.with_visible(false);
    }
    match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => viewport = viewport.with_icon(Arc::new(icon)),
        Err(e) => log::warn!("could not load app icon: {e}"),
    }
    let mut options = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size(size)
            .with_min_inner_size(size * 0.6),
        ..Default::default()
    };
    #[cfg(target_os = "linux")]
    if use_x11 {
        use winit::platform::x11::EventLoopBuilderExtX11;
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_x11();
        }));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = &mut options;
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        options.renderer = eframe::Renderer::Glow;
    }
    eframe::run_native(
        "OpenMeeter",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx, config.settings.theme);
            let logo = load_logo(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx, backend, config, config_path, logo, can_hide)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Wayland doesn't let apps hide their windows, so with the tray on (where
/// closing hides the window) run through XWayland when it's available.
fn prefer_x11(tray: bool) -> bool {
    let has = |var| std::env::var_os(var).is_some_and(|v| !v.is_empty());
    cfg!(target_os = "linux") && tray && has("WAYLAND_DISPLAY") && has("DISPLAY")
}

/// Whether the window can be hidden (and brought back from the tray).
fn can_hide_window(use_x11: bool) -> bool {
    let wayland = cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    use_x11 || !wayland
}

/// The wordmark cropped out of the app icon, as a texture for the title row.
fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let icon = eframe::icon_data::from_png_bytes(ICON_PNG).ok()?;
    let full = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
    let (pos, size) = LOGO_CROP;
    let logo = full.region_by_pixels(pos, size);
    Some(ctx.load_texture("logo", logo, egui::TextureOptions::LINEAR))
}

/// Size of the design grid for a given strip/bus layout.
fn design_size(mixer: &Mixer) -> egui::Vec2 {
    vec2(strips_width(mixer) + RIGHT_W, DESIGN_H)
}

fn strips_width(mixer: &Mixer) -> f32 {
    mixer.strips.iter().map(|s| if s.kind == Kind::Hardware { HW_STRIP_W } else { VIRT_STRIP_W }).sum()
}

struct App {
    backend: Box<dyn AudioBackend>,
    config: Config,
    config_path: PathBuf,
    devices: Vec<DeviceInfo>,
    problems: Vec<String>,
    status: Option<String>,
    unsaved_since: Option<Instant>,
    meters: ballistics::Ballistics,
    logo: Option<egui::TextureHandle>,
    recorder: Recorder,
    tray: Option<tray::Tray>,
    windows: menu::Windows,
    macros_ui: macros::State,
    /// Global hotkeys for the macro buttons, or why they're unavailable.
    hotkeys: Result<crate::macros::Hotkeys, String>,
    /// Set by Shut Down so closing the window really quits even with the tray on.
    quitting: bool,
    /// Cached "run on startup" state (read from the system, which owns it).
    run_on_startup: bool,
    next_restart_check: Instant,
    /// Engine counters as last written to the log, and when to look again.
    logged_stats: String,
    next_stats_check: Instant,
    last_stats_log: Instant,
    restart_backoff: Duration,
    /// False on native Wayland, where closing can't hide to the tray and just quits.
    can_hide: bool,
}

impl App {
    fn new(
        ctx: &egui::Context,
        backend: Box<dyn AudioBackend>,
        config: Config,
        config_path: PathBuf,
        logo: Option<egui::TextureHandle>,
        can_hide: bool,
    ) -> Self {
        let mut app = Self {
            backend,
            config,
            config_path,
            devices: Vec::new(),
            problems: Vec::new(),
            status: None,
            unsaved_since: None,
            meters: ballistics::Ballistics::default(),
            logo,
            recorder: Recorder::new(None),
            tray: None,
            windows: menu::Windows::default(),
            macros_ui: macros::State::default(),
            hotkeys: {
                let ctx = ctx.clone();
                crate::macros::Hotkeys::new(move || ctx.request_repaint())
            },
            quitting: false,
            run_on_startup: crate::autostart::is_enabled(),
            next_restart_check: Instant::now() + RESTART_CHECK_MIN,
            logged_stats: String::new(),
            next_stats_check: Instant::now() + STATS_CHECK,
            last_stats_log: Instant::now(),
            restart_backoff: RESTART_CHECK_MIN,
            can_hide,
        };
        app.recorder = Recorder::new(app.backend.player());
        app.set_tray(ctx, app.config.settings.tray);
        if !app.config.settings.show_on_startup && app.tray.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        app.backend.set_buffer_ms(app.config.settings.buffer_ms);
        app.refresh_devices();
        app.migrate_virtual_bindings();
        app.apply();
        app
    }

    /// Which cable (by driver name) each virtual input and virtual bus is bound to:
    /// `(driver -> strip label, driver -> bus key)`.
    fn cable_users(&self) -> (HashMap<String, String>, HashMap<String, String>) {
        let driver = |id: &Option<DeviceId>| {
            let device = self.devices.iter().find(|d| Some(&d.id) == id.as_ref())?;
            driver_name(&device.name).map(str::to_owned)
        };
        let mixer = &self.config.mixer;
        let strips = mixer.strips.iter().filter(|s| s.kind == Kind::Virtual);
        let buses = mixer.buses.iter().filter(|b| b.kind == Kind::Virtual);
        (
            strips.filter_map(|s| Some((driver(&s.device)?, s.label.clone()))).collect(),
            buses.filter_map(|b| Some((driver(&b.device)?, b.key.clone()))).collect(),
        )
    }

    /// Older configs stored the engine-side endpoint for virtual devices (a virtual
    /// input on "CABLE Output"). Rewrite them to the side apps use, which the UI shows.
    fn migrate_virtual_bindings(&mut self) {
        let devices = &self.devices;
        let swap = |binding: &mut Option<DeviceId>, want: Direction| {
            let Some(device) = binding.as_ref().and_then(|id| devices.iter().find(|d| &d.id == id)) else { return };
            if device.is_virtual && device.direction != want
                && let Some(partner) = cable_partner(devices, device)
            {
                log::info!("config: {} -> {}", device.name, partner.name);
                *binding = Some(partner.id.clone());
            }
        };
        let mixer = &mut self.config.mixer;
        for strip in mixer.strips.iter_mut().filter(|s| s.kind == Kind::Virtual) {
            swap(&mut strip.device, Direction::Playback);
        }
        for bus in mixer.buses.iter_mut().filter(|b| b.kind == Kind::Virtual) {
            swap(&mut bus.device, Direction::Capture);
        }
    }

    fn refresh_devices(&mut self) {
        match self.backend.devices() {
            Ok(devices) => {
                log::info!("{} audio devices found", devices.len());
                for d in &devices {
                    log::debug!("  {:?} {} virtual={}", d.direction, d.name, d.is_virtual);
                }
                self.devices = devices;
            }
            Err(e) => {
                log::error!("device enumeration failed: {e}");
                self.status = Some(format!("Device list unavailable: {e}"));
            }
        }
        self.problems = self.backend.diagnostics();
    }

    fn apply(&mut self) {
        // The status line reflects the latest routing result, so fixed problems disappear.
        self.status = match self.backend.apply(&self.config.mixer.to_graph()) {
            Ok(()) => None,
            Err(e) => Some(format!("Routing: {e}")),
        };
    }

    fn save_if_due(&mut self, force: bool) {
        let Some(since) = self.unsaved_since else { return };
        if force || since.elapsed() >= SAVE_DEBOUNCE {
            match config::save(&self.config, &self.config_path) {
                Ok(()) => self.unsaved_since = None,
                Err(e) => self.status = Some(format!("Saving config failed: {e}")),
            }
        }
    }

    /// Logo, status line and Menu button.
    fn title_bar(&mut self, ui: &mut Ui, o: Pos2, width: f32) {
        let p = ui.painter().clone();
        match &self.logo {
            Some(logo) => {
                let [w, h] = logo.size();
                let size = vec2(LOGO_HEIGHT * w as f32 / h as f32, LOGO_HEIGHT);
                let rect = egui::Rect::from_min_size(o + vec2(10.0, 17.5 - LOGO_HEIGHT / 2.0), size);
                p.image(logo.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            None => {
                text(&p, o + vec2(10.0, 17.5), Align2::LEFT_CENTER, "OPENMEETER", 14.0, theme::colors().text);
            }
        }

        let setup_warning = default_output_warning(&self.devices, &self.config.mixer);
        let (message, color) = match (self.problems.first(), &setup_warning, &self.status) {
            (Some(problem), _, _) => (problem.as_str(), theme::colors().orange),
            (None, Some(warning), _) => (warning.as_str(), theme::colors().orange),
            (None, None, Some(status)) => (status.as_str(), theme::colors().text_dim),
            (None, None, None) => ("", theme::colors().text_dim),
        };
        let msg_rect = egui::Rect::from_min_max(o + vec2(240.0, 6.0), o + vec2(width - 130.0, 30.0));
        p.with_clip_rect(msg_rect).text(msg_rect.left_center(), Align2::LEFT_CENTER, message, theme::font(12.0), color);

        let menu_rect = egui::Rect::from_min_size(o + vec2(width - 116.0, 6.0), vec2(106.0, 24.0));
        let menu = widgets::button(ui, menu_rect, Id::new("menu"), "Menu", false, theme::colors().text);
        if self.config.settings.lock_ui {
            let badge = egui::pos2(menu_rect.left() - 10.0, menu_rect.center().y);
            text(&p, badge, Align2::RIGHT_CENTER, "LOCKED", 12.0, theme::colors().orange);
        }
        let state = menu::MenuState {
            settings: &self.config.settings,
            run_on_startup: self.run_on_startup,
            tray_available: cfg!(any(windows, target_os = "linux")),
            cassette_loaded: self.recorder.has_file(),
        };
        let action = Popup::menu(&menu).show(|ui| menu::contents(ui, &state)).and_then(|r| r.inner);
        if let Some(action) = action {
            self.handle_menu(ui.ctx(), action);
        }
    }

    fn handle_menu(&mut self, ctx: &egui::Context, action: menu::MenuAction) {
        use menu::MenuAction as A;
        let settings = &mut self.config.settings;
        match action {
            A::RestartEngine => self.restart_engine(),
            A::SetAutoRestart(mode) => settings.auto_restart = mode,
            A::EjectCassette => self.recorder.eject(),
            A::LoadSettings => {
                if let Some(path) = settings_dialog().set_title("Load Settings").pick_file() {
                    match config::load_preset(&path) {
                        Ok(preset) => {
                            self.config.mixer = preset.mixer;
                            settings.theme = preset.theme;
                            theme::apply(ctx, preset.theme);
                            self.status = Some(format!("Loaded {}", path.display()));
                        }
                        Err(e) => self.status = Some(format!("{e:#}")),
                    }
                }
            }
            A::ChooseStartupPreset => {
                if let Some(path) = settings_dialog().set_title("Load Settings on Startup").pick_file() {
                    settings.startup_preset = Some(path);
                }
            }
            A::ClearStartupPreset => settings.startup_preset = None,
            A::SaveSettings => {
                let name = format!("OpenMeeter {}.json", chrono::Local::now().format("%Y-%m-%d"));
                if let Some(path) = settings_dialog().set_title("Save Settings").set_file_name(name).save_file() {
                    self.status = Some(match config::save_preset(&self.config.mixer, settings.theme, &path) {
                        Ok(()) => format!("Saved {}", path.display()),
                        Err(e) => format!("{e:#}"),
                    });
                }
            }
            A::ResetSettings => self.windows.confirm_reset = true,
            A::SetTray(on) => {
                settings.tray = on;
                self.set_tray(ctx, on);
            }
            A::SetRunOnStartup(on) => {
                if let Err(e) = crate::autostart::set_enabled(on) {
                    self.status = Some(format!("Couldn't change startup setting: {e}"));
                }
                self.run_on_startup = crate::autostart::is_enabled();
            }
            A::SetShowOnStartup(on) => settings.show_on_startup = on,
            A::SetAlwaysOnTop(on) => {
                settings.always_on_top = on;
                let level = if on { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            }
            A::SetLockUi(on) => settings.lock_ui = on,
            A::OpenSystemSettings => self.windows.system_settings = true,
            A::OpenRecorderOptions => self.windows.recorder_options = true,
            A::OpenMacros => self.macros_ui.open = true,
            A::RefreshDevices => self.refresh_devices(),
            A::ShutDown => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.unsaved_since.get_or_insert_with(Instant::now);
    }

    fn set_tray(&mut self, ctx: &egui::Context, on: bool) {
        if !on {
            self.tray = None;
            return;
        }
        if self.tray.is_none() {
            match tray::Tray::new(ctx, ICON_PNG) {
                Ok(t) => self.tray = Some(t),
                Err(e) => {
                    log::warn!("no tray icon: {e}");
                    // Never leave the window hidden without a way back.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
            }
        }
    }

    /// Write the engine's counters (underruns, skipped backlog, device errors) to
    /// the log when they change, so a glitch can be traced afterwards.
    fn log_engine_stats(&mut self) {
        if Instant::now() < self.next_stats_check {
            return;
        }
        self.next_stats_check = Instant::now() + STATS_CHECK;
        let stats = self.backend.stats();
        // The drift reading always moves; only the counters and errors count as news.
        let counters = |line: &String| line.split(", ").filter(|part| !part.starts_with("clock drift")).collect::<Vec<_>>().join(", ");
        let mut errors: Vec<String> = self.backend.node_errors().into_iter().map(|(key, e)| format!("{key}: {e}")).collect();
        errors.sort();
        let news = stats.iter().map(counters).chain(errors).collect::<Vec<_>>().join(" | ");
        if news != self.logged_stats || self.last_stats_log.elapsed() >= STATS_HEARTBEAT {
            log::info!("audio: {}", stats.join(" | "));
            self.logged_stats = news;
            self.last_stats_log = Instant::now();
        }
    }

    fn restart_engine(&mut self) {
        log::info!("restarting the audio engine");
        self.refresh_devices();
        self.backend.restart_engine();
        self.apply();
    }

    /// Auto Restart Audio Engine: when a device the user chose is failing but is
    /// present again, restart the engine. Checks back off while it keeps failing.
    fn auto_restart(&mut self) {
        let mode = self.config.settings.auto_restart;
        if mode == AutoRestart::Off || Instant::now() < self.next_restart_check {
            return;
        }
        let watched = |key: &String| mode == AutoRestart::AllDevices || key == "A1";
        let failing: Vec<String> = self
            .backend
            .node_errors()
            .into_iter()
            // A virtual input disabled for sharing a cable isn't a device failure.
            .filter(|(key, error)| watched(key) && !error.starts_with("Disabled:"))
            .map(|(key, _)| key)
            .collect();
        if failing.is_empty() {
            self.restart_backoff = RESTART_CHECK_MIN;
            self.next_restart_check = Instant::now() + RESTART_CHECK_MIN;
            return;
        }

        self.refresh_devices();
        let mixer = &self.config.mixer;
        let bound = mixer.strips.iter().map(|s| (&s.key, &s.device)).chain(mixer.buses.iter().map(|b| (&b.key, &b.device)));
        let recovered = bound
            .filter(|(key, _)| failing.contains(key))
            .any(|(_, device)| device.as_ref().is_some_and(|id| self.devices.iter().any(|d| &d.id == id)));
        if recovered {
            log::info!("auto restart: {} back, restarting audio engine", failing.join(", "));
            self.backend.restart_engine();
            self.apply();
            self.restart_backoff = (self.restart_backoff * 2).min(RESTART_CHECK_MAX);
        }
        self.next_restart_check = Instant::now() + self.restart_backoff;
    }

    /// Tray clicks and close-to-tray. Runs from `logic`, which eframe calls even
    /// while the window is hidden in the tray.
    fn tray_and_close(&mut self, ctx: &egui::Context) {
        let commands = self.tray.as_ref().map(tray::Tray::poll).unwrap_or_default();
        for command in commands {
            match command {
                tray::TrayCommand::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                tray::TrayCommand::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.quitting && self.tray.is_some() && self.can_hide {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    /// The menu's windows (settings, recorder options, reset confirmation).
    fn menu_windows(&mut self, ctx: &egui::Context) {
        let path = self.config_path.display().to_string();
        let (streams, errors) = if self.windows.system_settings {
            (self.backend.stream_info(), self.backend.node_errors())
        } else {
            Default::default()
        };
        let exclusive_mode = self.backend.capabilities().exclusive_mode;
        let info = menu::SystemInfo { devices: &self.devices, streams: &streams, errors: &errors, config_path: &path, exclusive_mode };
        if menu::system_settings(ctx, &mut self.windows.system_settings, &mut self.config.settings, &mut self.config.mixer, &info) {
            self.backend.set_buffer_ms(self.config.settings.buffer_ms);
            self.apply();
        }
        let (inputs, buses) = armables(&self.config.mixer);
        let recorder = &mut self.config.mixer.recorder;
        if let Some(action) = recorder_options::window(ctx, &mut self.windows.recorder_options, recorder, &inputs, &buses) {
            self.handle_recorder(action);
        }
        let targets = macro_targets(&self.config.mixer);
        let no_errors = HashMap::new();
        let info = macros::Info {
            targets: &targets,
            errors: self.hotkeys.as_ref().map_or(&no_errors, |h| &h.errors),
            unavailable: self.hotkeys.as_ref().err().map(String::as_str),
        };
        if let Some(i) = macros::window(ctx, &mut self.macros_ui, &mut self.config.settings.macros, &info) {
            self.run_macro(i);
        }
        if self.windows.confirm_reset
            && let Some(reset) = menu::confirm_reset(ctx)
        {
            self.windows.confirm_reset = false;
            if reset {
                self.config.mixer = Mixer::default();
            }
        }
    }

    /// Register changed hotkeys and run the macros whose hotkeys were pressed.
    fn poll_hotkeys(&mut self) {
        let Ok(hotkeys) = &mut self.hotkeys else { return };
        hotkeys.sync(&self.config.settings.macros, self.macros_ui.capturing.is_some());
        for i in hotkeys.fired() {
            self.run_macro(i);
        }
    }

    fn run_macro(&mut self, i: usize) {
        use crate::macros::MacroAction as M;
        let Some(m) = self.config.settings.macros.get(i) else { return };
        log::info!("macro: {}", m.name);
        match m.action.clone() {
            M::RestartEngine => self.restart_engine(),
            M::PlayClip { path: Some(path) } => self.recorder.play_file(path),
            M::PlayClip { path: None } => self.status = Some(format!("Macro \"{}\" has no sound clip chosen", m.name)),
            M::StopPlayback => self.recorder.stop_playback(),
            M::ToggleRecording => {
                let labels = self.config.mixer.node_labels();
                self.recorder.toggle_record(&mut *self.backend, &self.config.mixer.recorder, &labels);
            }
            M::ToggleMute { key } => {
                let mixer = &mut self.config.mixer;
                let mute = mixer.strips.iter_mut().find(|s| s.key == key).map(|s| &mut s.mute);
                let mute = mute.or_else(|| mixer.buses.iter_mut().find(|b| b.key == key).map(|b| &mut b.mute));
                if let Some(mute) = mute {
                    *mute = !*mute;
                    self.apply();
                    self.unsaved_since.get_or_insert_with(Instant::now);
                }
            }
        }
    }

    fn draw(&mut self, ui: &mut Ui) {
        let design = design_size(&self.config.mixer);
        fit_zoom(ui.ctx(), design);
        let screen = ui.ctx().content_rect();
        let o = (screen.center() - design / 2.0).max(screen.min);
        // Swallow clicks on empty background so they don't fall through to anything.
        ui.interact(screen, Id::new("background"), Sense::hover());

        self.title_bar(ui, o, design.x);

        let dt = ui.input(|i| i.stable_dt).min(0.1);
        self.meters.update(&self.backend.levels(), dt);
        let errors = self.backend.node_errors();
        let (strip_cables, bus_cables) = self.cable_users();
        let ctx = Ctx {
            devices: &self.devices,
            meters: &self.meters,
            errors: &errors,
            bus_cables: &bus_cables,
            strip_cables: &strip_cables,
            can_create_virtual: self.backend.capabilities().create_virtual_devices,
        };
        let recorder_display = self.recorder.display(&self.config.mixer.recorder);
        let mixer = &mut self.config.mixer;
        let bus_keys: Vec<String> = mixer.buses.iter().map(|b| b.key.clone()).collect();
        let locked = self.config.settings.lock_ui;
        let recorder_action = ui.scope(|ui| {
            if locked {
                // Controls ignore input; dim only slightly so the mixer stays readable.
                ui.visuals_mut().disabled_alpha = 0.8;
                ui.disable();
            }

            let mut x = 0.0;
            let mut edges = Vec::new();
            let mut hw_n = 0;
            let mut virt_j = 0;
            let mut virt_start = None;
            let kinds: Vec<Kind> = mixer.strips.iter().map(|s| s.kind).collect();
            for (i, strip) in mixer.strips.iter_mut().enumerate() {
                match strip.kind {
                    Kind::Hardware => {
                        hw_n += 1;
                        panels::hardware_strip(ui, o, x, hw_n, strip, &bus_keys, &ctx);
                        x += HW_STRIP_W;
                    }
                    Kind::Virtual => {
                        virt_start.get_or_insert(x);
                        panels::virtual_strip(ui, o, x, virt_j, strip, &bus_keys, &ctx);
                        virt_j += 1;
                        x += VIRT_STRIP_W;
                    }
                }
                // Between two virtual strips the line starts below their shared title.
                let inside_group = kinds.get(i + 1) == Some(&Kind::Virtual) && strip.kind == Kind::Virtual;
                let top = if inside_group { panels::GROUP_SEPARATOR_TOP } else { TITLE_H + 4.0 };
                edges.push((x, top));
            }
            if let Some(start) = virt_start {
                panels::virtual_inputs_title(ui, o, start, x - start);
            }
            edges.pop();

            panels::hardware_out_header(ui, o, x, &mut mixer.buses, &ctx);
            let recorder_action = panels::recorder(ui, o, x, &mut mixer.recorder, &bus_keys, &recorder_display);
            panels::master_section(ui, o, x, &mut mixer.buses, &ctx);
            panels::separators(ui, o, &edges, x);
            recorder_action
        })
        .inner;

        if let Some(action) = recorder_action {
            self.handle_recorder(action);
        }
    }

    fn handle_recorder(&mut self, action: RecorderAction) {
        let labels = self.config.mixer.node_labels();
        let settings = &mut self.config.mixer.recorder;
        let backend = &mut *self.backend;
        match action {
            RecorderAction::Load => {
                let picked = rfd::FileDialog::new()
                    .set_title("Load audio file")
                    .add_filter("Audio", &["wav", "mp3", "flac", "ogg", "m4a", "aac", "aif", "aiff"])
                    .pick_file();
                if let Some(path) = picked {
                    self.recorder.load(path);
                }
            }
            RecorderAction::Rewind => self.recorder.skip(false),
            RecorderAction::Forward => self.recorder.skip(true),
            RecorderAction::PlayPause => self.recorder.play_pause(),
            RecorderAction::Stop => self.recorder.stop(backend),
            RecorderAction::Record => self.recorder.toggle_record(backend, settings, &labels),
            RecorderAction::OpenOptions => self.windows.recorder_options = true,
            RecorderAction::ChooseFolder => {
                let start = recorder::recordings_folder(settings);
                if let Some(folder) = rfd::FileDialog::new().set_title("Recordings folder").set_directory(&start).pick_folder() {
                    settings.folder = Some(folder);
                }
            }
            RecorderAction::OpenFolder => {
                let folder = recorder::recordings_folder(settings);
                let opened = std::fs::create_dir_all(&folder).and_then(|()| open_in_file_manager(&folder));
                if let Err(e) = opened {
                    self.status = Some(format!("Couldn't open {}: {e}", folder.display()));
                }
            }
        }
    }
}

fn cable_of(devices: &[DeviceInfo], id: &Option<DeviceId>) -> Option<String> {
    let device = devices.iter().find(|d| Some(&d.id) == id.as_ref())?;
    if !device.is_virtual {
        return None;
    }
    driver_name(&device.name).map(str::to_owned)
}

/// One cable can be a virtual input or a B bus, not both. When the user gives a
/// cable to one side, take it away from the other instead of leaving a conflict.
fn move_claimed_cables(devices: &[DeviceInfo], before: &Mixer, mixer: &mut Mixer) {
    let newly = |now: &Option<DeviceId>, old: Option<&Option<DeviceId>>| if Some(now) != old { cable_of(devices, now) } else { None };
    let claimed_by_buses: Vec<String> = mixer
        .buses
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind == Kind::Virtual)
        .filter_map(|(i, b)| newly(&b.device, before.buses.get(i).map(|o| &o.device)))
        .collect();
    let claimed_by_strips: Vec<String> = mixer
        .strips
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == Kind::Virtual)
        .filter_map(|(i, s)| newly(&s.device, before.strips.get(i).map(|o| &o.device)))
        .collect();

    for strip in mixer.strips.iter_mut().filter(|s| s.kind == Kind::Virtual) {
        if cable_of(devices, &strip.device).is_some_and(|c| claimed_by_buses.contains(&c)) {
            log::info!("{} unassigned: its cable moved to a B bus", strip.label);
            strip.device = None;
        }
    }
    for bus in mixer.buses.iter_mut().filter(|b| b.kind == Kind::Virtual) {
        if cable_of(devices, &bus.device).is_some_and(|c| claimed_by_strips.contains(&c)) {
            log::info!("{} unassigned: its cable moved to a virtual input", bus.key);
            bus.device = None;
        }
    }
}

/// Warn when Windows' default output is the cable a B bus feeds: every app's sound
/// then goes into that cable, so programs recording the bus hear it all.
fn default_output_warning(devices: &[DeviceInfo], mixer: &Mixer) -> Option<String> {
    let default_out = devices.iter().find(|d| d.is_default && d.direction == Direction::Playback && d.is_virtual)?;
    let cable = driver_name(&default_out.name)?;
    let bus = mixer.buses.iter().find(|b| b.kind == Kind::Virtual && cable_of(devices, &b.device).as_deref() == Some(cable))?;
    Some(format!(
        "Windows' default output is {}, so all app sound goes into the cable {} feeds. Set your default output to your speakers in Windows Sound settings.",
        default_out.name, bus.key
    ))
}

/// Input strips and buses as the Recorder Options arming buttons show them.
fn armables(mixer: &Mixer) -> (Vec<recorder_options::Armable>, Vec<recorder_options::Armable>) {
    let (mut hw, mut virt) = (0, 0);
    let inputs = mixer
        .strips
        .iter()
        .map(|s| {
            let (kind, n) = match s.kind {
                Kind::Hardware => {
                    hw += 1;
                    ("Physical", hw)
                }
                Kind::Virtual => {
                    virt += 1;
                    ("Virtual", virt)
                }
            };
            let name = if s.label == crate::model::default_strip_label(s.kind, n) {
                if s.kind == Kind::Hardware { format!("Input #{n}") } else { format!("Input {n}") }
            } else {
                s.label.clone()
            };
            recorder_options::Armable { key: s.key.clone(), kind, name }
        })
        .collect();
    let buses = mixer
        .buses
        .iter()
        .map(|b| {
            let kind = if b.kind == Kind::Hardware { "Physical" } else { "Virtual" };
            recorder_options::Armable { key: b.key.clone(), kind, name: format!("BUS {}", b.key) }
        })
        .collect();
    (inputs, buses)
}

/// What a mute macro can target: every strip (by label) and bus (by key).
fn macro_targets(mixer: &Mixer) -> Vec<(String, String)> {
    let strips = mixer.strips.iter().map(|s| (s.key.clone(), s.label.clone()));
    strips.chain(mixer.buses.iter().map(|b| (b.key.clone(), format!("Bus {}", b.key)))).collect()
}

fn settings_dialog() -> rfd::FileDialog {
    let dir = directories::UserDirs::new().and_then(|d| d.document_dir().map(|p| p.join("OpenMeeter")));
    let dialog = rfd::FileDialog::new().add_filter("OpenMeeter settings", &["json"]);
    match dir {
        Some(dir) if dir.exists() => dialog.set_directory(dir),
        _ => dialog,
    }
}

fn open_in_file_manager(folder: &std::path::Path) -> std::io::Result<()> {
    let program = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(program).arg(folder).spawn().map(|_| ())
}

/// Scale the whole UI so the design grid fits the window, like a fixed-size skin.
fn fit_zoom(ctx: &egui::Context, design: egui::Vec2) {
    // Window size in points at zoom 1.0, independent of the current zoom.
    let base = ctx.content_rect().size() * ctx.zoom_factor();
    let zoom = (base.x / design.x).min(base.y / design.y).clamp(0.5, 3.0);
    if (zoom - ctx.zoom_factor()).abs() > 0.005 {
        ctx.set_zoom_factor(zoom);
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tray_and_close(ctx);
        // Hotkeys and the clips they load must work while the window is hidden.
        self.poll_hotkeys();
        self.log_engine_stats();
        self.recorder.poll(&mut *self.backend, &self.config.mixer.recorder);
        if self.recorder.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.auto_restart();
        let before = self.config.mixer.clone();
        let settings_before = self.config.settings.clone();
        self.menu_windows(ui.ctx());

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::colors().bg))
            .show(ui, |ui| self.draw(ui));

        if self.config.mixer != before {
            move_claimed_cables(&self.devices, &before, &mut self.config.mixer);
            self.apply();
            self.unsaved_since.get_or_insert_with(Instant::now);
        }
        if self.config.settings != settings_before {
            self.unsaved_since.get_or_insert_with(Instant::now);
        }
        self.save_if_due(false);

        ui.ctx().request_repaint_after(Duration::from_millis(1000 / METER_FPS));
    }

    fn on_exit(&mut self) {
        // Finalise an in-progress recording so the WAV header is valid.
        self.recorder.stop_recording(&mut *self.backend);
        self.save_if_due(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str, direction: Direction, is_default: bool) -> DeviceInfo {
        DeviceInfo { id: DeviceId(id.into()), name: name.into(), direction, channels: 2, is_virtual: name.contains("VB-Audio"), is_default }
    }

    fn devices(default_out_is_cable: bool) -> Vec<DeviceInfo> {
        vec![
            dev("out", "CABLE Output (VB-Audio Virtual Cable)", Direction::Capture, false),
            dev("in", "CABLE Input (VB-Audio Virtual Cable)", Direction::Playback, default_out_is_cable),
            dev("spk", "Speakers (Focusrite USB Audio)", Direction::Playback, !default_out_is_cable),
        ]
    }

    #[test]
    fn giving_a_cable_to_b1_takes_it_from_the_virtual_input() {
        let d = devices(false);
        let mut before = Mixer::default();
        before.strips[3].device = Some(DeviceId("in".into())); // Virtual Input 1 = CABLE Input
        let mut after = before.clone();
        after.buses[3].device = Some(DeviceId("out".into())); // user picks CABLE Output for B1
        move_claimed_cables(&d, &before, &mut after);
        assert_eq!(after.buses[3].device, Some(DeviceId("out".into())));
        assert_eq!(after.strips[3].device, None, "virtual input gave up the cable");
    }

    #[test]
    fn giving_a_cable_to_a_virtual_input_takes_it_from_b1() {
        let d = devices(false);
        let mut before = Mixer::default();
        before.buses[3].device = Some(DeviceId("out".into()));
        let mut after = before.clone();
        after.strips[3].device = Some(DeviceId("in".into()));
        move_claimed_cables(&d, &before, &mut after);
        assert_eq!(after.buses[3].device, None);
        assert_eq!(after.strips[3].device, Some(DeviceId("in".into())));
    }

    #[test]
    fn warns_when_windows_plays_into_a_b_bus_cable() {
        let mut mixer = Mixer::default();
        mixer.buses[3].device = Some(DeviceId("out".into()));
        assert!(default_output_warning(&devices(true), &mixer).is_some());
        assert!(default_output_warning(&devices(false), &mixer).is_none());
    }
}
