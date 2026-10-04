//! Macro buttons: actions bound to global hotkeys (e.g. Ctrl+F12 restarts the
//! audio engine, Numpad7 plays a sound clip), like Voicemeeter's Macro Buttons.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex, mpsc};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::{Deserialize, Serialize};

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
    /// Play an audio file through the recorder's playback buses, from the start.
    PlayClip { path: Option<PathBuf> },
    StopPlayback,
    ToggleRecording,
    /// Mute or unmute a strip or bus, by key.
    ToggleMute { key: String },
}

impl MacroAction {
    /// One of each kind, for the action picker.
    pub fn kinds() -> [MacroAction; 5] {
        [
            MacroAction::RestartEngine,
            MacroAction::PlayClip { path: None },
            MacroAction::StopPlayback,
            MacroAction::ToggleRecording,
            MacroAction::ToggleMute { key: String::new() },
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            MacroAction::RestartEngine => "Restart Audio Engine",
            MacroAction::PlayClip { .. } => "Play Sound Clip",
            MacroAction::StopPlayback => "Stop Playback",
            MacroAction::ToggleRecording => "Start/Stop Recording",
            MacroAction::ToggleMute { .. } => "Toggle Mute",
        }
    }

    pub fn same_kind(&self, other: &MacroAction) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
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
    }
}
