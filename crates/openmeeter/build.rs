//! Embeds the app icon into the Windows executable (Explorer, shortcuts, taskbar
//! pins). The window icon itself is set at runtime from the same PNG.

use std::path::PathBuf;

const ICON_PNG: &str = "../../images/icon.png";
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed={ICON_PNG}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo")).join("icon.ico");
    let icon = image::open(ICON_PNG).expect("images/icon.png in the repository").into_rgba8();
    let frames = ICO_SIZES
        .iter()
        .map(|&size| {
            let resized = image::imageops::resize(&icon, size, size, image::imageops::FilterType::Lanczos3);
            image::codecs::ico::IcoFrame::as_png(resized.as_raw(), size, size, image::ExtendedColorType::Rgba8)
                .expect("encode icon frame")
        })
        .collect::<Vec<_>>();
    let file = std::fs::File::create(&out).expect("create icon.ico");
    image::codecs::ico::IcoEncoder::new(file).encode_images(&frames).expect("write icon.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(out.to_str().expect("utf-8 OUT_DIR"));
    if let Err(e) = res.compile() {
        // Missing resource compiler shouldn't break the build; the window icon still works.
        println!("cargo:warning=could not embed exe icon: {e}");
    }
}
