//! Voicemeeter-style mixer model: input strips route to output buses.
//! Hardware buses are A1..An, virtual buses B1..Bn.

use std::collections::BTreeSet;
use std::path::PathBuf;

use openmeeter_backend::{DeviceId, GraphNode, PLAYER_KEY, RoutingGraph, VirtualDevice};
use serde::{Deserialize, Serialize};

pub const MIN_GAIN_DB: f32 = -60.0;
pub const MAX_GAIN_DB: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Hardware,
    Virtual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    pub key: String,
    pub label: String,
    pub kind: Kind,
    pub device: Option<DeviceId>,
    pub gain_db: f32,
    pub mute: bool,
    pub solo: bool,
    /// Keys of the buses this strip feeds.
    pub routes: BTreeSet<String>,
    #[serde(default)]
    pub mono: bool,

    // Hardware strips: Intellipan pad and Comp/Gate knobs (0..10).
    #[serde(default)]
    pub pan_mode: PanMode,
    /// Pad position, both axes 0..1; (0.5, 0.0) is centred at the bottom.
    #[serde(default = "centre_bottom")]
    pub pan: [f32; 2],
    #[serde(default)]
    pub comp: f32,
    #[serde(default)]
    pub gate: f32,

    // Virtual strips: 3-band EQ (dB) and surround panner.
    #[serde(default)]
    pub eq_treble: f32,
    #[serde(default)]
    pub eq_mid: f32,
    #[serde(default)]
    pub eq_bass: f32,
    /// Surround position, both axes 0..1; (0.5, 0.5) is centred.
    #[serde(default = "centre")]
    pub surround: [f32; 2],
    /// Voicemeeter's "M.C" (mix to centre) button on the first virtual strip.
    #[serde(default)]
    pub mix_centre: bool,
    /// Voicemeeter's "K" (karaoke) button: 0 = off, 1..=4 = mode.
    #[serde(default)]
    pub karaoke: u8,
}

fn centre() -> [f32; 2] {
    [0.5, 0.5]
}

fn centre_bottom() -> [f32; 2] {
    [0.5, 0.0]
}

pub const KARAOKE_MODES: u8 = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PanMode {
    #[default]
    Voice,
    ColorPanel,
    Modulation,
    FxPanel,
}

impl PanMode {
    pub const ALL: [PanMode; 4] = [PanMode::Voice, PanMode::ColorPanel, PanMode::Modulation, PanMode::FxPanel];

    pub fn label(self) -> &'static str {
        match self {
            PanMode::Voice => "VOICE",
            PanMode::ColorPanel => "Color Panel",
            PanMode::Modulation => "MODULATION",
            PanMode::FxPanel => "Fx Panel",
        }
    }
}

/// Voicemeeter bus modes (the button at the top of each master strip).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusMode {
    #[default]
    Normal,
    MixDownA,
    MixDownB,
    StereoRepeat,
    Composite,
    TvMix,
    UpMix21,
    UpMix41,
    UpMix61,
    CenterOnly,
    LfeOnly,
    RearOnly,
}

impl BusMode {
    pub const ALL: [BusMode; 12] = [
        BusMode::Normal,
        BusMode::MixDownA,
        BusMode::MixDownB,
        BusMode::StereoRepeat,
        BusMode::Composite,
        BusMode::TvMix,
        BusMode::UpMix21,
        BusMode::UpMix41,
        BusMode::UpMix61,
        BusMode::CenterOnly,
        BusMode::LfeOnly,
        BusMode::RearOnly,
    ];

    /// Two-line label as shown on the button.
    pub fn label(self) -> &'static str {
        match self {
            BusMode::Normal => "Normal\nmode",
            BusMode::MixDownA => "MIX\ndown A",
            BusMode::MixDownB => "MIX\ndown B",
            BusMode::StereoRepeat => "Stereo\nRepeat",
            BusMode::Composite => "Compo-\nsite",
            BusMode::TvMix => "TV\nMix",
            BusMode::UpMix21 => "UpMix\n2.1",
            BusMode::UpMix41 => "UpMix\n4.1",
            BusMode::UpMix61 => "UpMix\n6.1",
            BusMode::CenterOnly => "Center\nonly",
            BusMode::LfeOnly => "LFE\nonly",
            BusMode::RearOnly => "Rear\nonly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bus {
    pub key: String,
    pub kind: Kind,
    pub device: Option<DeviceId>,
    pub gain_db: f32,
    pub mute: bool,
    #[serde(default)]
    pub mode: BusMode,
    #[serde(default)]
    pub mono: bool,
    #[serde(default)]
    pub eq: bool,
    /// Stereo reverse: swap left and right (the mono button's third state).
    #[serde(default)]
    pub reverse: bool,
    /// Monitoring Synchro Delay, in milliseconds.
    #[serde(default)]
    pub delay_ms: f32,
}

pub const MAX_BUS_DELAY_MS: f32 = 500.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mixer {
    pub strips: Vec<Strip>,
    pub buses: Vec<Bus>,
    #[serde(default)]
    pub recorder: RecorderSettings,
}

/// The cassette recorder: what it records, where, and where playback goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderSettings {
    /// Buses the player plays into (the A1..B2 buttons beside the recorder, as in Voicemeeter).
    pub playback_buses: BTreeSet<String>,
    /// Record armed input strips (pre-fader) or armed buses (post-fader).
    pub source: RecordSource,
    /// Input strips (by key) armed for pre-fader recording.
    pub armed_inputs: BTreeSet<String>,
    /// Buses armed for post-fader recording.
    pub record_buses: BTreeSet<String>,
    pub format: RecordFormat,
    /// Where recordings go; `None` means Documents\OpenMeeter.
    pub folder: Option<PathBuf>,
    /// File name prefix, followed by the date and time.
    pub prefix: String,
    /// 1 = mono (left and right averaged), 2 = stereo.
    pub channels: u16,
    /// One file per armed source instead of one mixed file.
    pub multitrack: bool,
    /// Start playing as soon as a file is loaded.
    pub play_on_load: bool,
    pub loop_playback: bool,
    /// Level of the player as it feeds the buses.
    pub playback_gain_db: f32,
    /// Stop recording automatically after this many minutes.
    pub stop_after_minutes: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordSource {
    PreFaderInputs,
    #[default]
    PostFaderOutputs,
}

impl RecorderSettings {
    /// Node keys that get recorded under the current source.
    pub fn armed(&self) -> &BTreeSet<String> {
        match self.source {
            RecordSource::PreFaderInputs => &self.armed_inputs,
            RecordSource::PostFaderOutputs => &self.record_buses,
        }
    }
}

impl Default for RecorderSettings {
    fn default() -> Self {
        Self {
            playback_buses: BTreeSet::from(["A1".to_string()]),
            source: RecordSource::default(),
            armed_inputs: BTreeSet::from(["hw1".to_string()]),
            record_buses: BTreeSet::from(["A1".to_string()]),
            format: RecordFormat::default(),
            folder: None,
            prefix: "OpenMeeter".to_string(),
            channels: 2,
            multitrack: false,
            play_on_load: true,
            loop_playback: false,
            playback_gain_db: 0.0,
            stop_after_minutes: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordFormat {
    Pcm16,
    #[default]
    Pcm24,
    Float32,
}

impl RecordFormat {
    pub const ALL: [RecordFormat; 3] = [RecordFormat::Pcm16, RecordFormat::Pcm24, RecordFormat::Float32];

    pub fn label(self) -> &'static str {
        match self {
            RecordFormat::Pcm16 => "WAV 16-bit",
            RecordFormat::Pcm24 => "WAV 24-bit",
            RecordFormat::Float32 => "WAV 32-bit float",
        }
    }
}

impl Default for Mixer {
    /// Voicemeeter Banana layout: 3 hardware + 2 virtual inputs, A1-A3 + B1-B2.
    fn default() -> Self {
        Self::with_layout(3, 2, 3, 2)
    }
}

/// The name a strip gets by default ("Hardware Input 2", "Virtual Input 1").
pub fn default_strip_label(kind: Kind, n: usize) -> String {
    match kind {
        Kind::Hardware => format!("Hardware Input {n}"),
        Kind::Virtual => format!("Virtual Input {n}"),
    }
}

/// The system device created for the `n`th virtual strip: apps play into it.
pub fn virtual_strip_device(key: &str, n: usize) -> VirtualDevice {
    VirtualDevice { name: format!("openmeeter_{key}"), description: format!("OpenMeeter {}", default_strip_label(Kind::Virtual, n)) }
}

/// The system device created for a virtual bus: apps record from it.
pub fn virtual_bus_device(key: &str) -> VirtualDevice {
    VirtualDevice { name: format!("openmeeter_{key}"), description: format!("OpenMeeter Output {key}") }
}

impl Mixer {
    pub fn with_layout(hw_in: usize, virt_in: usize, hw_out: usize, virt_out: usize) -> Self {
        let strip = |key: String, label: String, kind| Strip {
            key,
            label,
            kind,
            device: None,
            gain_db: 0.0,
            mute: false,
            solo: false,
            routes: BTreeSet::from(["A1".to_string()]),
            mono: false,
            pan_mode: PanMode::default(),
            pan: centre_bottom(),
            comp: 0.0,
            gate: 0.0,
            eq_treble: 0.0,
            eq_mid: 0.0,
            eq_bass: 0.0,
            surround: centre(),
            mix_centre: false,
            karaoke: 0,
        };
        let bus = |key: String, kind| Bus {
            key,
            kind,
            device: None,
            gain_db: 0.0,
            mute: false,
            mode: BusMode::default(),
            mono: false,
            eq: false,
            reverse: false,
            delay_ms: 0.0,
        };

        let strips = (1..=hw_in)
            .map(|i| strip(format!("hw{i}"), default_strip_label(Kind::Hardware, i), Kind::Hardware))
            .chain((1..=virt_in).map(|i| strip(format!("v{i}"), default_strip_label(Kind::Virtual, i), Kind::Virtual)))
            .collect();
        let buses = (1..=hw_out)
            .map(|i| bus(format!("A{i}"), Kind::Hardware))
            .chain((1..=virt_out).map(|i| bus(format!("B{i}"), Kind::Virtual)))
            .collect();
        Self { strips, buses, recorder: RecorderSettings::default() }
    }

    /// Display names by node key, as the recorder shows them: strips by their
    /// label, buses by their key.
    pub fn node_labels(&self) -> std::collections::HashMap<String, String> {
        let strips = self.strips.iter().map(|s| (s.key.clone(), s.label.clone()));
        strips.chain(self.buses.iter().map(|b| (b.key.clone(), b.key.clone()))).collect()
    }

    /// Translate the mixer into backend routing intent.
    pub fn to_graph(&self) -> RoutingGraph {
        let any_solo = self.strips.iter().any(|s| s.solo);
        let mut virtual_strips = 0;

        let player = GraphNode {
            key: PLAYER_KEY.to_string(),
            device: None,
            create_virtual: None,
            gain_db: self.recorder.playback_gain_db,
            mute: false,
            mono: false,
            reverse: false,
            delay_ms: 0.0,
        };
        let sources = self
            .strips
            .iter()
            .map(|s| GraphNode {
                key: s.key.clone(),
                device: s.device.clone(),
                create_virtual: (s.kind == Kind::Virtual).then(|| {
                    virtual_strips += 1;
                    virtual_strip_device(&s.key, virtual_strips)
                }),
                gain_db: s.gain_db,
                mute: s.mute || (any_solo && !s.solo),
                mono: s.mono,
                reverse: false,
                delay_ms: 0.0,
            })
            .chain(std::iter::once(player))
            .collect();
        let sinks = self
            .buses
            .iter()
            .map(|b| GraphNode {
                key: b.key.clone(),
                device: b.device.clone(),
                create_virtual: (b.kind == Kind::Virtual).then(|| virtual_bus_device(&b.key)),
                gain_db: b.gain_db,
                mute: b.mute,
                mono: b.mono,
                reverse: b.reverse,
                delay_ms: b.delay_ms.clamp(0.0, MAX_BUS_DELAY_MS),
            })
            .collect();
        let routes = self
            .strips
            .iter()
            .flat_map(|s| s.routes.iter().map(|b| (s.key.clone(), b.clone())))
            .chain(self.recorder.playback_buses.iter().map(|b| (PLAYER_KEY.to_string(), b.clone())))
            .filter(|(_, bus)| self.buses.iter().any(|b| &b.key == bus))
            .collect();

        RoutingGraph { sources, sinks, routes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_banana_layout() {
        let m = Mixer::default();
        let keys: Vec<_> = m.buses.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, ["A1", "A2", "A3", "B1", "B2"]);
        assert_eq!(m.strips.len(), 5);
        assert_eq!(m.strips.iter().filter(|s| s.kind == Kind::Virtual).count(), 2);
    }

    #[test]
    fn solo_mutes_other_strips() {
        let mut m = Mixer::default();
        m.strips[1].solo = true;
        let g = m.to_graph();
        let muted: Vec<_> = g.sources.iter().map(|s| s.mute).collect();
        assert_eq!(muted, [true, false, true, true, true, false], "the player isn't a strip, so solo leaves it alone");
    }

    #[test]
    fn routes_to_missing_buses_are_dropped() {
        let mut m = Mixer::with_layout(1, 0, 1, 0);
        m.strips[0].routes.insert("B7".into());
        m.recorder.playback_buses.insert("B7".into());
        let routes = m.to_graph().routes;
        assert_eq!(routes, [("hw1".to_string(), "A1".to_string()), (PLAYER_KEY.to_string(), "A1".to_string())]);
    }

    #[test]
    fn only_virtual_nodes_request_creation() {
        let g = Mixer::default().to_graph();
        assert!(g.sources[0].create_virtual.is_none());
        let v2 = g.sources[4].create_virtual.as_ref().unwrap();
        assert_eq!((v2.name.as_str(), v2.description.as_str()), ("openmeeter_v2", "OpenMeeter Virtual Input 2"));
        assert_eq!(g.sinks[3].create_virtual.as_ref().unwrap().description, "OpenMeeter Output B1");
    }
}
