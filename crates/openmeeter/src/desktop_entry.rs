//! Linux desktop integration: docks and task bars find an app's icon through a
//! `.desktop` file whose `StartupWMClass` matches the window's app ID, so the
//! GUI installs one (plus the icon) for the current user.

use std::path::Path;

/// The window's app ID (Wayland) / WM_CLASS (X11), and the desktop file's name.
pub const APP_ID: &str = "openmeeter";

/// Write `~/.local/share/applications/openmeeter.desktop` and the icon, if they
/// are missing or out of date (e.g. the binary moved).
pub fn install(icon_png: &[u8]) {
    if let Err(e) = try_install(icon_png) {
        log::warn!("could not install the desktop entry: {e}");
    }
}

fn try_install(icon_png: &[u8]) -> anyhow::Result<()> {
    let dirs = directories::BaseDirs::new().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let share = dirs.data_local_dir();

    let icon_path = share.join("icons/hicolor/256x256/apps").join(format!("{APP_ID}.png"));
    let icon = image::load_from_memory(icon_png)?.resize(256, 256, image::imageops::FilterType::Lanczos3);
    let mut png = Vec::new();
    icon.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    write_if_changed(&icon_path, &png)?;

    let exe = std::env::current_exe()?;
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=OpenMeeter\n\
         Comment=Voicemeeter-style audio mixer\n\
         Exec=\"{}\"\n\
         Icon={APP_ID}\n\
         StartupWMClass={APP_ID}\n\
         Categories=AudioVideo;Audio;Mixer;\n\
         Terminal=false\n",
        exe.display()
    );
    write_if_changed(&share.join("applications").join(format!("{APP_ID}.desktop")), entry.as_bytes())
}

fn write_if_changed(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == contents) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().expect("file path has a parent"))?;
    std::fs::write(path, contents)?;
    log::info!("installed {}", path.display());
    Ok(())
}
