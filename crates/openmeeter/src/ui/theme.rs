//! Colours and fonts for the Voicemeeter-style skin.
//!
//! The palette can be customised from the config file: `settings.theme` maps
//! colour names to hex strings (`"bg": "#2f4050"`). Colours left out keep their
//! defaults; `openmeeter theme` prints every name with its default.
//!
//! Three colours are shortcuts for a whole family (see [`Palette::derived`]):
//! `bg` for the surfaces (panels, menus, the recorder), `accent` for everything
//! lit (buttons, faders, knob rings) and `boost` for a fader above 0 dB. Setting
//! one recolours its family in matching shades, unless a member is set on its own.

use std::collections::BTreeMap;
use std::sync::RwLock;

use eframe::egui::{self, Color32, FontId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Declares the palette once: the struct, its defaults, and name-based access
/// for the config file.
macro_rules! palette {
    ($($(#[$doc:meta])* $name:ident = ($r:expr, $g:expr, $b:expr),)*) => {
        /// Every themable colour. Get the active one with [`colors`].
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Palette {
            $($(#[$doc])* pub $name: Color32,)*
        }

        impl Palette {
            pub const DEFAULT: Palette = Palette { $($name: Color32::from_rgb($r, $g, $b),)* };

            /// Every colour with its config name, in declaration order.
            fn entries(&self) -> Vec<(&'static str, Color32)> {
                vec![$((stringify!($name), self.$name),)*]
            }

            /// Set a colour by its config name; false if there's no such colour.
            fn set(&mut self, name: &str, color: Color32) -> bool {
                match name {
                    $(stringify!($name) => self.$name = color,)*
                    _ => return false,
                }
                true
            }
        }
    };
}

palette! {
    /// Window and menu background. Setting it derives the other surfaces from it
    /// (panels, separators, the recorder body and buttons...), unless they are set too.
    bg = (47, 64, 80),
    /// Inset panels: pan pads, knob faces.
    panel = (36, 50, 63),
    panel_stroke = (70, 90, 108),
    separator = (62, 81, 99),

    text = (200, 214, 225),
    text_dim = (140, 160, 176),
    text_faint = (105, 125, 142),
    /// Values that should stand out: fader gains, active settings.
    text_bright = (255, 255, 255),

    /// Button and knob outlines.
    outline = (98, 118, 135),
    /// One colour for everything "lit": setting it derives `green`, the fader
    /// colours and `knob_on` from it, unless they are set too.
    accent = (120, 222, 165),
    /// Lit buttons (A1, B1, mono...) and accents.
    green = (120, 222, 165),
    blue = (110, 205, 240),
    orange = (245, 170, 70),
    red = (235, 80, 70),
    /// A knob turned away from its reset position: its ring and its value.
    knob_on = (93, 159, 132),
    knob_value_on = (248, 99, 77),
    /// The pad handles (Intellipan, surround panner).
    handle = (140, 170, 195),
    /// The heavy line between the input strips and the master section.
    divider = (30, 42, 54),

    fader_track = (126, 200, 160),
    /// Fader track while the strip is muted.
    fader_track_dim = (88, 140, 112),
    fader_thumb_ring = (160, 225, 190),
    fader_thumb = (92, 160, 124),
    /// One colour for boosting: setting it derives the fader's colours above
    /// 0 dB and `knob_value_on` from it, unless they are set too.
    boost = (248, 99, 77),
    /// The fader parts above 0 dB, where it boosts the signal.
    fader_boost_track = (248, 99, 77),
    fader_boost_track_dim = (158, 76, 66),
    fader_boost_thumb_ring = (252, 146, 128),
    fader_boost_thumb = (206, 72, 54),

    meter_off = (58, 34, 38),
    meter_green = (90, 215, 120),
    meter_yellow = (235, 205, 60),
    meter_red = (235, 70, 60),

    /// The recorder: its body, tape window and display.
    recorder_body = (26, 34, 42),
    recorder_tape = (48, 60, 72),
    recorder_screen = (218, 226, 212),
    /// Text on the recorder's display, and its colour for errors and recording.
    recorder_ink = (40, 50, 55),
    recorder_ink_alert = (190, 30, 30),
    /// The recorder's transport buttons: idle, hovered, active.
    transport = (58, 72, 86),
    transport_hover = (68, 84, 100),
    transport_active = (80, 98, 116),
    /// Recorder options: an armed input or bus, while its mode is active.
    armed = (240, 100, 85),
    /// Recorder options: an armed input or bus of the inactive mode.
    armed_idle = (120, 70, 68),
    /// Recorder options: the top line of an active armed button.
    armed_text = (255, 225, 220),
    /// Recorder options: the record-mode dot.
    record_dot = (230, 20, 20),
    /// Recorder options: value boxes, and the inset playback panel.
    value_box = (34, 46, 58),
    value_box_hover = (46, 60, 74),
    inset = (24, 32, 40),
}

impl Default for Palette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Palette {
    /// The defaults with each family recoloured from its driving colour, in the
    /// lighter and darker shades the default members have. A driver left at its
    /// default leaves its family exactly as it is.
    pub fn derived(bg: Color32, accent: Color32, boost: Color32) -> Palette {
        let mut p = Palette::DEFAULT;
        if bg != p.bg {
            p.bg = bg;
            p.panel = shade(bg, -0.22);
            p.panel_stroke = shade(bg, 0.13);
            p.separator = shade(bg, 0.09);
            p.divider = shade(bg, -0.35);
            p.value_box = shade(bg, -0.28);
            p.value_box_hover = shade(bg, -0.05);
            p.inset = shade(bg, -0.50);
            p.recorder_body = shade(bg, -0.46);
            p.recorder_tape = shade(bg, -0.05);
            p.transport = shade(bg, 0.05);
            p.transport_hover = shade(bg, 0.10);
            p.transport_active = shade(bg, 0.17);
        }
        if accent != p.accent {
            p.accent = accent;
            p.green = accent;
            p.fader_track = shade(accent, -0.10);
            p.fader_track_dim = shade(accent, -0.37);
            p.fader_thumb_ring = shade(accent, 0.30);
            p.fader_thumb = shade(accent, -0.28);
            p.knob_on = shade(accent, -0.28);
        }
        if boost != p.boost {
            p.boost = boost;
            p.fader_boost_track = boost;
            p.fader_boost_track_dim = shade(boost, -0.36);
            p.fader_boost_thumb_ring = shade(boost, 0.30);
            p.fader_boost_thumb = shade(boost, -0.17);
            p.knob_value_on = boost;
        }
        p
    }

    /// The whole palette as config-file JSON, e.g. to copy colours from.
    pub fn to_json(self) -> String {
        let lines: Vec<String> = self.entries().into_iter().map(|(name, c)| format!("  \"{name}\": \"{}\"", to_hex(c))).collect();
        format!("{{
{}
}}", lines.join(",
"))
    }
}

/// Only colours the user chose are saved: those that differ from the defaults,
/// or from what the family's driving colour derives. So the config file holds
/// just the overrides, later default changes still apply, and colours that
/// follow `bg`, `accent` or `boost` keep following it.
impl Serialize for Palette {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The driving colours are compared with their defaults, everything else with what they give.
        let mut baseline = Palette::derived(self.bg, self.accent, self.boost);
        (baseline.bg, baseline.accent, baseline.boost) = (Palette::DEFAULT.bg, Palette::DEFAULT.accent, Palette::DEFAULT.boost);
        let changed = self.entries().into_iter().zip(baseline.entries()).filter(|(mine, base)| mine != base);
        serializer.collect_map(changed.map(|((name, color), _)| (name, to_hex(color))))
    }
}

/// Unknown names and unreadable colours are skipped with a warning rather than
/// failing, so a typo in the theme can't make the whole config unreadable.
impl<'de> Deserialize<'de> for Palette {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        // The driving colours first: they set the starting point the others override.
        let driver = |name: &str, default: Color32| map.get(name).and_then(|v| v.as_str()).and_then(parse_hex).unwrap_or(default);
        let d = Palette::DEFAULT;
        let mut palette = Palette::derived(driver("bg", d.bg), driver("accent", d.accent), driver("boost", d.boost));
        for (name, value) in map {
            match value.as_str().and_then(parse_hex) {
                Some(color) if palette.set(&name, color) => {}
                Some(_) => log::warn!("theme: no colour named \"{name}\" (run `openmeeter theme` for the list)"),
                None => log::warn!("theme: {name} should be a colour like \"#2f4050\", not {value}"),
            }
        }
        Ok(palette)
    }
}

/// Lighten (`amount` > 0, towards white) or darken (< 0, towards black) a colour.
fn shade(c: Color32, amount: f32) -> Color32 {
    let target = if amount > 0.0 { 255.0 } else { 0.0 };
    let t = amount.abs().min(1.0);
    let mix = |x: u8| (x as f32 + (target - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
}

fn to_hex(c: Color32) -> String {
    // Color32 stores premultiplied alpha; the file holds plain sRGB.
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`; the `#` is optional.
fn parse_hex(text: &str) -> Option<Color32> {
    let hex = text.trim().trim_start_matches('#');
    if !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => {
            let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|d| d * 17);
            Some(Color32::from_rgb(digit(0)?, digit(1)?, digit(2)?))
        }
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

static ACTIVE: RwLock<Palette> = RwLock::new(Palette::DEFAULT);

/// The active palette.
pub fn colors() -> Palette {
    ACTIVE.read().map_or(Palette::DEFAULT, |p| *p)
}

/// Make `palette` the active one and restyle egui's own widgets to match.
pub fn apply(ctx: &egui::Context, palette: Palette) {
    if let Ok(mut active) = ACTIVE.write() {
        *active = palette;
    }
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = palette.bg;
    // Menus and dropdowns sit on the same background as the window.
    visuals.window_fill = palette.bg;
    visuals.window_stroke.color = palette.panel_stroke;
    visuals.widgets.noninteractive.bg_stroke.color = palette.separator;
    visuals.override_text_color = Some(palette.text);
    visuals.selection.bg_fill = palette.fader_thumb;
    ctx.set_visuals(visuals);
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(parse_hex("#2f4050"), Some(Color32::from_rgb(47, 64, 80)));
        assert_eq!(parse_hex("2F4050"), Some(Color32::from_rgb(47, 64, 80)));
        assert_eq!(parse_hex("#fa0"), Some(Color32::from_rgb(255, 170, 0)));
        assert_eq!(to_hex(Color32::from_rgb(47, 64, 80)), "#2f4050");
        let translucent = Color32::from_rgba_unmultiplied(255, 0, 0, 128);
        assert_eq!(parse_hex(&to_hex(translucent)), Some(translucent));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("teal"), None);
    }

    #[test]
    fn the_default_theme_saves_as_nothing() {
        assert_eq!(serde_json::to_string(&Palette::DEFAULT).unwrap(), "{}");
    }

    #[test]
    fn overrides_load_and_save_and_typos_keep_defaults() {
        let json = r##"{"bg": "#101820", "fader_track": "#ff00ff", "no_such_colour": "#000000", "text": "blue", "red": 7}"##;
        let palette: Palette = serde_json::from_str(json).unwrap();
        assert_eq!(palette.bg, Color32::from_rgb(16, 24, 32));
        assert_eq!(palette.fader_track, Color32::from_rgb(255, 0, 255));
        assert_eq!(palette.text, Palette::DEFAULT.text, "an unreadable colour keeps its default");
        assert_eq!(palette.red, Palette::DEFAULT.red);
        assert_eq!(serde_json::to_string(&palette).unwrap(), r##"{"bg":"#101820","fader_track":"#ff00ff"}"##);
    }

    #[test]
    fn accent_recolours_its_family_unless_overridden() {
        let p: Palette = serde_json::from_str(r##"{"accent": "#d113b7"}"##).unwrap();
        let accent = Color32::from_rgb(0xd1, 0x13, 0xb7);
        assert_eq!((p.accent, p.green), (accent, accent), "lit buttons take the accent itself");
        for derived in [p.fader_track, p.fader_thumb, p.fader_thumb_ring, p.fader_track_dim, p.knob_on] {
            assert_ne!(derived, accent);
            assert!(derived.r() > derived.g() && derived.b() > derived.g(), "{derived:?} keeps the accent's hue");
        }
        assert!(p.fader_thumb_ring.r() > accent.r() && p.fader_thumb.r() < accent.r(), "ring lighter, thumb darker");
        assert_eq!(p.bg, Palette::DEFAULT.bg, "nothing outside the family changes");
        assert_eq!(serde_json::to_string(&p).unwrap(), r##"{"accent":"#d113b7"}"##, "derived colours aren't saved, so they keep following");

        // An explicit colour wins over the accent, whichever comes first in the file.
        let json = r##"{"fader_track": "#00ff00", "accent": "#d113b7"}"##;
        let p: Palette = serde_json::from_str(json).unwrap();
        assert_eq!(p.fader_track, Color32::from_rgb(0, 255, 0));
        let d = Palette::DEFAULT;
        assert_eq!(p.fader_thumb, Palette::derived(d.bg, accent, d.boost).fader_thumb);
        assert_eq!(serde_json::to_string(&p).unwrap(), r##"{"accent":"#d113b7","fader_track":"#00ff00"}"##);
    }

    #[test]
    fn bg_and_boost_recolour_their_families() {
        let p: Palette = serde_json::from_str(r##"{"bg": "#1e1b26", "boost": "#ffcc00", "panel": "#000000"}"##).unwrap();
        let (bg, d) = (Color32::from_rgb(0x1e, 0x1b, 0x26), Palette::DEFAULT);
        assert_eq!((p.bg, p.panel), (bg, Color32::BLACK), "an explicit member wins");
        assert!(p.recorder_body.r() < bg.r() && p.panel_stroke.r() > bg.r(), "darker and lighter shades of bg");
        assert_ne!(p.transport, d.transport);
        assert_eq!((p.text, p.accent, p.fader_track), (d.text, d.accent, d.fader_track), "other families are untouched");

        let boost = Color32::from_rgb(0xff, 0xcc, 0x00);
        assert_eq!((p.fader_boost_track, p.knob_value_on), (boost, boost));
        assert_ne!(p.fader_boost_thumb, d.fader_boost_thumb);
        assert_eq!(serde_json::to_string(&p).unwrap(), r##"{"bg":"#1e1b26","panel":"#000000","boost":"#ffcc00"}"##);
    }

    #[test]
    fn without_an_accent_the_defaults_are_untouched() {
        let d = Palette::DEFAULT;
        assert_eq!(Palette::derived(d.bg, d.accent, d.boost), d);
        let p: Palette = serde_json::from_str(r##"{"green": "#ff0000"}"##).unwrap();
        assert_eq!(p.fader_track, Palette::DEFAULT.fader_track, "green alone doesn't move the faders");
    }

    #[test]
    fn the_printed_palette_lists_every_colour_and_loads_back() {
        let json = Palette::DEFAULT.to_json();
        assert!(json.contains("\"fader_boost_track\": \"#f8634d\""));
        assert_eq!(serde_json::from_str::<Palette>(&json).unwrap(), Palette::DEFAULT);
    }
}
