//! "Run on startup": a per-user startup entry, read live from the system so the
//! menu always shows the real state.

#[cfg(windows)]
mod imp {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "OpenMeeter";

    pub fn is_enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .and_then(|key| key.get_value::<String, _>(VALUE))
            .is_ok()
    }

    pub fn set_enabled(on: bool) -> std::io::Result<()> {
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)?;
        if on {
            let exe = std::env::current_exe()?;
            key.set_value(VALUE, &format!("\"{}\"", exe.display()))
        } else {
            match key.delete_value(VALUE) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    //! XDG autostart: ~/.config/autostart/openmeeter.desktop

    use std::path::PathBuf;

    fn entry() -> Option<PathBuf> {
        Some(directories::BaseDirs::new()?.config_dir().join("autostart").join("openmeeter.desktop"))
    }

    pub fn is_enabled() -> bool {
        entry().is_some_and(|p| p.exists())
    }

    pub fn set_enabled(on: bool) -> std::io::Result<()> {
        let path = entry().ok_or_else(|| std::io::Error::other("no config directory"))?;
        if on {
            let exe = std::env::current_exe()?;
            std::fs::create_dir_all(path.parent().expect("autostart dir"))?;
            let desktop =
                format!("[Desktop Entry]\nType=Application\nName=OpenMeeter\nExec=\"{}\"\nIcon=openmeeter\nX-GNOME-Autostart-enabled=true\n", exe.display());
            std::fs::write(path, desktop)
        } else {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }
}

pub use imp::{is_enabled, set_enabled};
