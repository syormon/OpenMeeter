//! Macro buttons: actions bound to global hotkeys (e.g. Ctrl+F12 restarts the
//! audio engine, Numpad7 plays a sound clip), like Voicemeeter's Macro Buttons.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex, mpsc};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::{Deserialize, Serialize};

use crate::model::{MAX_GAIN_DB, MIN_GAIN_DB, Mixer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    /// E.g. "Ctrl+F12" or "Numpad7"; `None` = only runs from its button.
    pub hotkey: Option<String>,
    pub action: MacroAction,
}

impl Default for Macro {
    fn default() -> Self {
        Self { name: "New macro".into(), hotkey: None, action: MacroAction::RestartEngine }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MacroAction {
    RestartEngine,
    /// Bring the window to the front, or hide it if it already is.
    ShowHideWindow,
    /// Replace the mixer (and theme) with a saved settings file, like Load Settings.
    LoadSettings { path: Option<PathBuf> },
    /// Run a Lua script from the scripts folder, by name (without ".lua").
    RunScript { name: String },

    // The recorder's transport.
    PlayPause,
    /// Stop and rewind playback, and end a recording.
    Stop,
    /// Stop and rewind playback, leaving a recording running.
    StopPlayback,
    Rewind,
    FastForward,
    ToggleRecording,
    /// Play an audio file through the recorder's playback buses, from the start.
    PlayClip { path: Option<PathBuf> },
    Eject,

    // Strips and buses, by key.
    /// Mute or unmute a strip or bus.
    ToggleMute { key: String },
    ToggleSolo { key: String },
    /// Switch a strip's A1/B1/... button.
    ToggleRoute { key: String, bus: String },
    ToggleMono { key: String },
    /// Move a strip or bus fader to `db`.
    SetGain { key: String, db: f32 },
    /// Move a strip or bus fader by `db` (negative turns it down).
    ChangeGain { key: String, db: f32 },
}

/// What a macro action acts on, for the target pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Targets {
    None,
    Strips,
    StripsAndBuses,
}

impl MacroAction {
    /// One of each kind, in picker order; `None` marks a separator.
    pub fn kinds() -> Vec<Option<MacroAction>> {
        let key = String::new;
        vec![
            Some(MacroAction::RestartEngine),
            Some(MacroAction::ShowHideWindow),
            Some(MacroAction::LoadSettings { path: None }),
            Some(MacroAction::RunScript { name: String::new() }),
            None,
            Some(MacroAction::PlayPause),
            Some(MacroAction::Stop),
            Some(MacroAction::StopPlayback),
            Some(MacroAction::Rewind),
            Some(MacroAction::FastForward),
            Some(MacroAction::ToggleRecording),
            Some(MacroAction::PlayClip { path: None }),
            Some(MacroAction::Eject),
            None,
            Some(MacroAction::ToggleMute { key: key() }),
            Some(MacroAction::ToggleSolo { key: key() }),
            Some(MacroAction::ToggleRoute { key: key(), bus: key() }),
            Some(MacroAction::ToggleMono { key: key() }),
            Some(MacroAction::SetGain { key: key(), db: 0.0 }),
            Some(MacroAction::ChangeGain { key: key(), db: 3.0 }),
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            MacroAction::RestartEngine => "Restart Audio Engine",
            MacroAction::ShowHideWindow => "Show/Hide Window",
            MacroAction::LoadSettings { .. } => "Load Settings",
            MacroAction::RunScript { .. } => "Run Script",
            MacroAction::PlayPause => "Play/Pause",
            MacroAction::Stop => "Stop",
            MacroAction::StopPlayback => "Stop Playback",
            MacroAction::Rewind => "Rewind",
            MacroAction::FastForward => "Fast Forward",
            MacroAction::ToggleRecording => "Start/Stop Recording",
            MacroAction::PlayClip { .. } => "Play Sound Clip",
            MacroAction::Eject => "Eject Cassette",
            MacroAction::ToggleMute { .. } => "Toggle Mute",
            MacroAction::ToggleSolo { .. } => "Toggle Solo",
            MacroAction::ToggleRoute { .. } => "Toggle Output (A1, B1...)",
            MacroAction::ToggleMono { .. } => "Toggle Mono",
            MacroAction::SetGain { .. } => "Set Fader",
            MacroAction::ChangeGain { .. } => "Fader Up/Down",
        }
    }

    pub fn hint(&self) -> &'static str {
        match self {
            MacroAction::ShowHideWindow => "Bring OpenMeeter to the front, or hide it if it's already there",
            MacroAction::LoadSettings { .. } => "Replace the mixer with a saved settings file, as Menu > Load Settings does",
            MacroAction::RunScript { .. } => "Run a Lua script (Menu > Macro Scripts) that can change anything Voicemeeter's API can",
            MacroAction::Stop => "Stop and rewind playback, and end a recording",
            MacroAction::StopPlayback => "Stop and rewind playback, leaving a recording running",
            MacroAction::PlayClip { .. } => "Play a file from the start through the recorder's playback buses",
            MacroAction::Rewind | MacroAction::FastForward => "Skip playback by 10 seconds",
            MacroAction::ToggleRoute { .. } => "Switch one of a strip's output buttons on or off",
            MacroAction::ChangeGain { .. } => "Move the fader by this much; negative turns it down",
            _ => "",
        }
    }

    pub fn targets(&self) -> Targets {
        match self {
            MacroAction::ToggleSolo { .. } | MacroAction::ToggleRoute { .. } => Targets::Strips,
            MacroAction::ToggleMute { .. } | MacroAction::ToggleMono { .. } | MacroAction::SetGain { .. } | MacroAction::ChangeGain { .. } => {
                Targets::StripsAndBuses
            }
            _ => Targets::None,
        }
    }

    pub fn same_kind(&self, other: &MacroAction) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// Carry the strip or bus over when switching to another kind of action.
    pub fn with_target_of(mut self, previous: &MacroAction) -> Self {
        if let (Some(from), Some(to)) = (previous.key(), self.key_mut()) {
            *to = from.to_string();
        }
        self
    }

    pub fn key(&self) -> Option<&str> {
        match self {
            MacroAction::ToggleMute { key }
            | MacroAction::ToggleSolo { key }
            | MacroAction::ToggleRoute { key, .. }
            | MacroAction::ToggleMono { key }
            | MacroAction::SetGain { key, .. }
            | MacroAction::ChangeGain { key, .. } => Some(key),
            _ => None,
        }
    }

    pub fn key_mut(&mut self) -> Option<&mut String> {
        match self {
            MacroAction::ToggleMute { key }
            | MacroAction::ToggleSolo { key }
            | MacroAction::ToggleRoute { key, .. }
            | MacroAction::ToggleMono { key }
            | MacroAction::SetGain { key, .. }
            | MacroAction::ChangeGain { key, .. } => Some(key),
            _ => None,
        }
    }

    /// Apply a strip or bus action to the mixer. `Ok(false)` for actions that
    /// aren't about the mixer; `Err` when the target is missing.
    pub fn apply_to(&self, mixer: &mut Mixer) -> Result<bool, String> {
        let missing = |key: &str| if key.is_empty() { "no strip or bus chosen".to_string() } else { format!("there is no {key} any more") };
        match self {
            MacroAction::ToggleSolo { key } => {
                let strip = mixer.strips.iter_mut().find(|s| s.key == *key).ok_or_else(|| missing(key))?;
                strip.solo = !strip.solo;
            }
            MacroAction::ToggleRoute { key, bus } => {
                if !mixer.buses.iter().any(|b| b.key == *bus) {
                    return Err(if bus.is_empty() { "no output chosen".into() } else { format!("there is no {bus} any more") });
                }
                let strip = mixer.strips.iter_mut().find(|s| s.key == *key).ok_or_else(|| missing(key))?;
                if !strip.routes.remove(bus) {
                    strip.routes.insert(bus.clone());
                }
            }
            MacroAction::ToggleMute { key } | MacroAction::ToggleMono { key } | MacroAction::SetGain { key, .. } | MacroAction::ChangeGain { key, .. } => {
                let (mute, mono, gain) = if let Some(s) = mixer.strips.iter_mut().find(|s| s.key == *key) {
                    (&mut s.mute, &mut s.mono, &mut s.gain_db)
                } else if let Some(b) = mixer.buses.iter_mut().find(|b| b.key == *key) {
                    (&mut b.mute, &mut b.mono, &mut b.gain_db)
                } else {
                    return Err(missing(key));
                };
                match self {
                    MacroAction::ToggleMute { .. } => *mute = !*mute,
                    MacroAction::ToggleMono { .. } => *mono = !*mono,
                    MacroAction::SetGain { db, .. } => *gain = db.clamp(MIN_GAIN_DB, MAX_GAIN_DB),
                    MacroAction::ChangeGain { db, .. } => *gain = (*gain + db).clamp(MIN_GAIN_DB, MAX_GAIN_DB),
                    _ => unreachable!("matched above"),
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// The hotkey for a key press, or `None` for keys that can't be hotkeys.
/// `numpad` picks the keypad version of digits and + - * / . Enter.
pub fn hotkey_name(modifiers: eframe::egui::Modifiers, key: eframe::egui::Key, numpad: bool) -> Option<String> {
    let mut name = String::new();
    for (on, label) in [(modifiers.ctrl, "Ctrl"), (modifiers.shift, "Shift"), (modifiers.alt, "Alt")] {
        if on {
            name.push_str(label);
            name.push('+');
        }
    }
    name.push_str(&key_token(key, numpad)?);
    Some(name)
}

fn key_token(key: eframe::egui::Key, numpad: bool) -> Option<String> {
    use eframe::egui::Key as K;
    let name = key.name();
    let token = match key {
        K::Num0 | K::Num1 | K::Num2 | K::Num3 | K::Num4 | K::Num5 | K::Num6 | K::Num7 | K::Num8 | K::Num9 => {
            if numpad { format!("Numpad{name}") } else { name.to_string() }
        }
        K::Plus | K::Minus | K::Period | K::Slash | K::Enter if numpad => match key {
            K::Plus => "NumpadAdd",
            K::Minus => "NumpadSubtract",
            K::Period => "NumpadDecimal",
            K::Slash => "NumpadDivide",
            _ => "NumpadEnter",
        }
        .to_string(),
        K::ArrowUp => "Up".into(),
        K::ArrowDown => "Down".into(),
        K::ArrowLeft => "Left".into(),
        K::ArrowRight => "Right".into(),
        K::Minus => "-".into(),
        K::Equals => "=".into(),
        K::Period => ".".into(),
        K::Slash => "/".into(),
        K::Backslash => "\\".into(),
        K::Semicolon => ";".into(),
        K::Quote => "'".into(),
        K::Backtick => "`".into(),
        K::OpenBracket => "[".into(),
        K::CloseBracket => "]".into(),
        K::Comma => "Comma".into(),
        K::Plus => "NumpadAdd".into(),
        _ if name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic()) => name.to_string(),
        _ if name.starts_with('F') && name[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => name.to_string(),
        K::Space | K::Enter | K::Tab | K::Backspace | K::Insert | K::Delete | K::Home | K::End | K::PageUp | K::PageDown => {
            name.replace(' ', "")
        }
        _ => return None,
    };
    // Only offer what the hotkey library can register.
    HotKey::from_str(&token).ok().map(|_| token)
}

/// Whether `token` (e.g. "7" or "Ctrl+7") ends in a key that has a keypad twin.
pub fn has_numpad_twin(hotkey: &str) -> bool {
    let key = hotkey.rsplit('+').next().unwrap_or(hotkey);
    let key = key.strip_prefix("Numpad").unwrap_or(key);
    matches!(key, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "Add" | "Subtract" | "Decimal" | "Divide" | "Enter" | "-" | "." | "/")
}

/// Switch a hotkey between the keypad and main-keyboard version of its key.
pub fn set_numpad(hotkey: &str, numpad: bool) -> String {
    let (mods, key) = match hotkey.rfind('+') {
        Some(i) if i + 1 < hotkey.len() => hotkey.split_at(i + 1),
        _ => ("", hotkey),
    };
    let plain = match key.strip_prefix("Numpad") {
        Some("Add") => "NumpadAdd",
        Some("Subtract") => "-",
        Some("Decimal") => ".",
        Some("Divide") => "/",
        Some("Enter") => "Enter",
        Some(digit) => digit,
        None => key,
    };
    let key = if !numpad {
        plain.to_string()
    } else {
        match plain {
            "-" => "NumpadSubtract".into(),
            "." => "NumpadDecimal".into(),
            "/" => "NumpadDivide".into(),
            "Enter" => "NumpadEnter".into(),
            "NumpadAdd" => "NumpadAdd".into(),
            digit => format!("Numpad{digit}"),
        }
    };
    format!("{mods}{key}")
}

/// Whether the keypad version of `key` is physically held right now. egui reports
/// keypad digits as plain digits, so the hotkey capture asks the OS. Windows only;
/// elsewhere the user picks with the Numpad toggle.
pub fn numpad_held(key: eframe::egui::Key) -> bool {
    #[cfg(windows)]
    {
        use eframe::egui::Key as K;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        let vk = match key {
            K::Num0 => VK_NUMPAD0,
            K::Num1 => VK_NUMPAD1,
            K::Num2 => VK_NUMPAD2,
            K::Num3 => VK_NUMPAD3,
            K::Num4 => VK_NUMPAD4,
            K::Num5 => VK_NUMPAD5,
            K::Num6 => VK_NUMPAD6,
            K::Num7 => VK_NUMPAD7,
            K::Num8 => VK_NUMPAD8,
            K::Num9 => VK_NUMPAD9,
            K::Plus => VK_ADD,
            K::Minus => VK_SUBTRACT,
            K::Period => VK_DECIMAL,
            K::Slash => VK_DIVIDE,
            _ => return false,
        };
        // SAFETY: GetAsyncKeyState only reads keyboard state.
        unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        false
    }
}

/// The registered global hotkeys.
pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    /// Hotkey id -> indices of the macros it runs.
    targets: HashMap<u32, Vec<usize>>,
    /// The hotkey list last synced, to skip re-registering when nothing changed.
    synced: Vec<Option<String>>,
    /// Why a macro's hotkey couldn't be registered, by macro index.
    pub errors: HashMap<usize, String>,
}

impl Hotkeys {
    /// `wake` is called from the hotkey thread on every press so the UI loop runs
    /// even while the window is hidden.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let _ = PENDING.0.send(event);
            wake();
        }));
        Ok(Self { manager, registered: Vec::new(), targets: HashMap::new(), synced: Vec::new(), errors: HashMap::new() })
    }

    /// Register exactly the hotkeys of `macros` (none while `paused`, e.g. while the
    /// user is capturing a new combo, so the OS doesn't swallow it).
    pub fn sync(&mut self, macros: &[Macro], paused: bool) {
        let wanted: Vec<Option<String>> = if paused { Vec::new() } else { macros.iter().map(|m| m.hotkey.clone()).collect() };
        if wanted == self.synced {
            return;
        }
        let _ = self.manager.unregister_all(&self.registered);
        self.registered.clear();
        self.targets.clear();
        self.errors.clear();
        for (i, name) in wanted.iter().enumerate() {
            let Some(name) = name else { continue };
            let hotkey = match HotKey::from_str(name) {
                Ok(h) => h,
                Err(e) => {
                    self.errors.insert(i, format!("Invalid hotkey: {e}"));
                    continue;
                }
            };
            let targets = self.targets.entry(hotkey.id()).or_default();
            if targets.is_empty() {
                if let Err(e) = self.manager.register(hotkey) {
                    self.errors.insert(i, format!("{name} is taken by another app ({e})"));
                    self.targets.remove(&hotkey.id());
                    continue;
                }
                self.registered.push(hotkey);
            }
            self.targets.entry(hotkey.id()).or_default().push(i);
        }
        self.synced = wanted;
    }

    /// Indices of the macros whose hotkeys were pressed since the last call.
    pub fn fired(&self) -> Vec<usize> {
        let mut fired = Vec::new();
        let Ok(rx) = PENDING.1.lock() else { return fired };
        while let Ok(event) = rx.try_recv() {
            if event.state == HotKeyState::Pressed
                && let Some(targets) = self.targets.get(&event.id)
            {
                fired.extend(targets);
            }
        }
        fired
    }
}

/// Presses from the hotkey event handler (a global callback) to the UI thread.
type Pending = (mpsc::Sender<GlobalHotKeyEvent>, Mutex<mpsc::Receiver<GlobalHotKeyEvent>>);
static PENDING: LazyLock<Pending> = LazyLock::new(|| {
    let (tx, rx) = mpsc::channel();
    (tx, Mutex::new(rx))
});

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Key;
    use eframe::egui::Modifiers;

    #[test]
    fn key_presses_become_registrable_hotkeys() {
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(hotkey_name(ctrl, Key::F12, false).as_deref(), Some("Ctrl+F12"));
        assert_eq!(hotkey_name(Modifiers::default(), Key::Num7, true).as_deref(), Some("Numpad7"));
        assert_eq!(hotkey_name(Modifiers::default(), Key::Num7, false).as_deref(), Some("7"));
        let all = Modifiers { ctrl: true, shift: true, alt: true, ..Default::default() };
        assert_eq!(hotkey_name(all, Key::A, false).as_deref(), Some("Ctrl+Shift+Alt+A"));
        assert_eq!(hotkey_name(Modifiers::default(), Key::PageDown, false).as_deref(), Some("PageDown"));
        for name in ["Ctrl+F12", "Numpad7", "Ctrl+Shift+Alt+A", "PageDown", "Up", "NumpadAdd"] {
            assert!(HotKey::from_str(name).is_ok(), "{name} parses");
        }
    }

    #[test]
    fn numpad_toggle_round_trips() {
        assert!(has_numpad_twin("Ctrl+7") && has_numpad_twin("Numpad7") && !has_numpad_twin("Ctrl+F12"));
        assert_eq!(set_numpad("Ctrl+7", true), "Ctrl+Numpad7");
        assert_eq!(set_numpad("Ctrl+Numpad7", false), "Ctrl+7");
        assert_eq!(set_numpad("-", true), "NumpadSubtract");
        assert_eq!(set_numpad("NumpadSubtract", false), "-");
        assert_eq!(set_numpad("Shift+Enter", true), "Shift+NumpadEnter");
    }

    #[test]
    fn macros_load_from_json() {
        let json = r#"{"name":"Clip","hotkey":"Numpad7","action":{"PlayClip":{"path":"C:/clip.wav"}}}"#;
        let m: Macro = serde_json::from_str(json).unwrap();
        assert_eq!(m.action, MacroAction::PlayClip { path: Some("C:/clip.wav".into()) });
        let json = r#"{"name":"Mic","hotkey":null,"action":{"ToggleRoute":{"key":"hw1","bus":"A1"}}}"#;
        let m: Macro = serde_json::from_str(json).unwrap();
        assert_eq!(m.action, MacroAction::ToggleRoute { key: "hw1".into(), bus: "A1".into() });
    }

    #[test]
    fn mixer_actions_change_their_target() {
        let mut mixer = Mixer::default();
        let (strip, bus) = (mixer.strips[0].key.clone(), mixer.buses[0].key.clone());
        let routed = mixer.strips[0].routes.contains(&bus);

        let route = MacroAction::ToggleRoute { key: strip.clone(), bus: bus.clone() };
        assert_eq!(route.apply_to(&mut mixer), Ok(true));
        assert_eq!(mixer.strips[0].routes.contains(&bus), !routed);
        route.apply_to(&mut mixer).unwrap();
        assert_eq!(mixer.strips[0].routes.contains(&bus), routed, "a second press switches it back");

        MacroAction::ToggleSolo { key: strip.clone() }.apply_to(&mut mixer).unwrap();
        assert!(mixer.strips[0].solo);
        MacroAction::ToggleMute { key: bus.clone() }.apply_to(&mut mixer).unwrap();
        assert!(mixer.buses[0].mute);

        MacroAction::SetGain { key: strip.clone(), db: -6.0 }.apply_to(&mut mixer).unwrap();
        MacroAction::ChangeGain { key: strip.clone(), db: 2.5 }.apply_to(&mut mixer).unwrap();
        assert_eq!(mixer.strips[0].gain_db, -3.5);
        for _ in 0..10 {
            MacroAction::ChangeGain { key: strip.clone(), db: 3.0 }.apply_to(&mut mixer).unwrap();
        }
        assert_eq!(mixer.strips[0].gain_db, MAX_GAIN_DB, "faders stop at their ends");

        assert!(MacroAction::ToggleSolo { key: bus }.apply_to(&mut mixer).is_err(), "buses have no solo");
        assert!(MacroAction::ToggleMute { key: String::new() }.apply_to(&mut mixer).is_err());
        assert_eq!(MacroAction::PlayPause.apply_to(&mut mixer), Ok(false));
    }

    #[test]
    fn switching_actions_keeps_the_target() {
        let before = MacroAction::ToggleMute { key: "hw2".into() };
        let after = MacroAction::SetGain { key: String::new(), db: 0.0 }.with_target_of(&before);
        assert_eq!(after.key(), Some("hw2"));
        assert_eq!(MacroAction::PlayPause.with_target_of(&before), MacroAction::PlayPause);
    }
}
