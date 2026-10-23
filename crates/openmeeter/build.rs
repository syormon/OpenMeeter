//! Renders the app's artwork from the SVGs in `images/`:
//! - `icon2.svg`, the app icon: `$OUT_DIR/icon.png`, compiled into the app for the
//!   window, taskbar and tray (and the Linux desktop entry), and on Windows an
//!   `.ico` embedded in the executable (Explorer, shortcuts, taskbar pins);
//! - `icon.svg`, the wordmark: `$OUT_DIR/logo.png`, shown at the top left of the
//!   window.
//!
//! Text in the SVG is not rendered: that would depend on the fonts installed on
//! the build machine. Convert text to outlines in the SVG instead.

use std::path::{Path, PathBuf};

use image::RgbaImage;

const ICON_SVG: &str = "../../images/icon2.svg";
const LOGO_SVG: &str = "../../images/icon.svg";
/// Longest side of the rendered artwork, in pixels; the icon is a square of it.
const ICON_SIZE: u32 = 1024;
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed={ICON_SVG}");
    println!("cargo:rerun-if-changed={LOGO_SVG}");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));

    let icon = square(&render(Path::new(ICON_SVG)).unwrap_or_else(|e| panic!("{ICON_SVG}: {e}")));
    icon.save(out_dir.join("icon.png")).expect("write icon.png");
    let logo = render(Path::new(LOGO_SVG)).unwrap_or_else(|e| panic!("{LOGO_SVG}: {e}"));
    logo.save(out_dir.join("logo.png")).expect("write logo.png");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_exe_icon(&icon, &out_dir.join("icon.ico"));
    }
}

/// Rasterise the SVG, clipped to its canvas, with its longest side `ICON_SIZE`.
fn render(path: &Path) -> Result<RgbaImage, String> {
    use resvg::{tiny_skia, usvg};

    let svg = std::fs::read_to_string(path).map_err(|e| format!("can't be read ({e})"))?;
    // Photoshop labels embedded images "img/png", which isn't a media type; a
    // strict renderer skips them, losing the layers that hold the picture.
    let svg = svg.replace("\"data:img/", "\"data:image/");
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).map_err(|e| format!("isn't a valid SVG ({e})"))?;
    let size = tree.size();
    let scale = ICON_SIZE as f32 / size.width().max(size.height());
    let scaled = |side: f32| ((side * scale).round() as u32).clamp(1, ICON_SIZE);
    let (width, height) = (scaled(size.width()), scaled(size.height()));

    // A pixmap the size of the canvas clips whatever the artwork draws outside it.
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("no memory for the icon")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia holds premultiplied alpha; PNG and ICO want it straight.
    let straight = |p: &tiny_skia::PremultipliedColorU8| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    };
    let straight: Vec<u8> = pixmap.pixels().iter().flat_map(straight).collect();
    if straight.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
        return Err("rendered as an empty image (does it only contain text, or images in an unsupported format?)".into());
    }
    RgbaImage::from_raw(width, height, straight).ok_or_else(|| "rendered to the wrong size".into())
}

/// Centre artwork on a square `ICON_SIZE` tile. If its canvas wasn't square, its
/// background colour (when it has one) fills the rest, so the icon is a full tile
/// rather than a banner.
fn square(art: &RgbaImage) -> RgbaImage {
    let corner = *art.get_pixel(0, 0);
    let background = if corner[3] == 255 { corner } else { image::Rgba([0, 0, 0, 0]) };
    let mut icon = RgbaImage::from_pixel(ICON_SIZE, ICON_SIZE, background);
    image::imageops::replace(&mut icon, art, ((ICON_SIZE - art.width()) / 2).into(), ((ICON_SIZE - art.height()) / 2).into());
    icon
}

fn embed_exe_icon(icon: &RgbaImage, ico: &Path) {
    let frames = ICO_SIZES
        .iter()
        .map(|&size| {
            let resized = image::imageops::resize(icon, size, size, image::imageops::FilterType::Lanczos3);
            image::codecs::ico::IcoFrame::as_png(resized.as_raw(), size, size, image::ExtendedColorType::Rgba8)
                .expect("encode icon frame")
        })
        .collect::<Vec<_>>();
    let file = std::fs::File::create(ico).expect("create icon.ico");
    image::codecs::ico::IcoEncoder::new(file).encode_images(&frames).expect("write icon.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("utf-8 OUT_DIR"));
    if let Err(e) = res.compile() {
        // Missing resource compiler shouldn't break the build; the window icon still works.
        println!("cargo:warning=could not embed exe icon: {e}");
    }
}
