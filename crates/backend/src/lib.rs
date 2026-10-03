//! The contract between the OpenMeeter app and a platform audio backend.
//!
//! The app describes *routing intent* as a [`RoutingGraph`]; each backend
//! decides how to realise it. On Linux that means creating PipeWire nodes and
//! links. On Windows it means configuring our own WASAPI mixing engine, with
//! VB-Cable providing the virtual endpoints.

pub mod engine;
mod mock;
mod player;
mod resample;

pub use mock::MockBackend;
pub use player::{ENGINE_CHANNELS, ENGINE_SAMPLE_RATE, PLAYER_KEY, Player, RecordingStream};

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, BackendError>;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("not implemented on this backend yet: {0}")]
    NotImplemented(&'static str),
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("{0}")]
    Platform(String),
}

/// Opaque, backend-specific stable identifier for an audio endpoint
/// (a PipeWire `node.name`, a Windows endpoint ID string, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(pub String);

/// Direction from the system's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Produces audio we can capture (microphone, line in, VB-Cable output).
    Capture,
    /// Consumes audio we play into (speakers, headphones, VB-Cable input).
    Playback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub direction: Direction,
    pub channels: u16,
    /// True for virtual endpoints (VB-Cable, PipeWire null sinks, ...).
    pub is_virtual: bool,
    /// The system default device for its direction (where apps play/record by default).
    #[serde(default)]
    pub is_default: bool,
}

/// The endpoint's driver, from the name suffix: "CABLE Output (VB-Audio Virtual Cable)"
/// gives "VB-Audio Virtual Cable".
pub fn driver_name(name: &str) -> Option<&str> {
    let start = name.rfind('(')?;
    name[start + 1..].strip_suffix(')')
}

/// A virtual cable shows up as two endpoints named after the same driver: a
/// playback side apps play into ("CABLE Input") and a capture side apps record
/// from ("CABLE Output"). Returns `device`'s other side, preferring the plain
/// stereo endpoint over variants like "CABLE In 16ch".
pub fn cable_partner<'a>(devices: &'a [DeviceInfo], device: &DeviceInfo) -> Option<&'a DeviceInfo> {
    let driver = driver_name(&device.name)?;
    devices
        .iter()
        .filter(|d| d.is_virtual && d.direction != device.direction && driver_name(&d.name) == Some(driver))
        .min_by_key(|d| (d.name.contains("16ch"), d.name.len()))
}

/// The endpoint to open for `id` in `direction`. The app names virtual devices by
/// the side apps use (a virtual input is "CABLE Input", a B bus is "CABLE Output"),
/// while the engine needs the opposite side, so cables are swapped for their partner.
pub fn resolve_endpoint<'a>(devices: &'a [DeviceInfo], id: &DeviceId, direction: Direction) -> Option<&'a DeviceInfo> {
    let device = devices.iter().find(|d| &d.id == id)?;
    if device.direction == direction {
        Some(device)
    } else if device.is_virtual {
        cable_partner(devices, device)
    } else {
        None
    }
}

/// What a backend can do, so the UI can hide features it can't provide.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    /// Backend can create virtual devices on demand (PipeWire). If false,
    /// virtual strips and buses must be bound to existing virtual devices (VB-Cable).
    pub create_virtual_devices: bool,
    /// Backend can move individual application streams between devices.
    pub per_app_routing: bool,
    /// Outputs can be opened exclusively (see [`GraphNode::exclusive`]).
    pub exclusive_mode: bool,
}

/// One endpoint of the mixer: an input strip (source) or an output bus (sink).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNode {
    /// Stable key from the app model, e.g. `"hw1"` or `"B2"`.
    pub key: String,
    /// Bound system device, if the user picked one.
    pub device: Option<DeviceId>,
    /// For virtual strips/buses on backends that create devices: the device to create.
    pub create_virtual: Option<VirtualDevice>,
    pub gain_db: f32,
    pub mute: bool,
    /// Downmix to mono (L and R both carry the average).
    pub mono: bool,
    /// Swap left and right (ignored when `mono` is set).
    pub reverse: bool,
    /// Output delay in milliseconds, e.g. to line speakers up with a stream.
    pub delay_ms: f32,
    /// Sinks only: open the device exclusively, bypassing the OS mixer (Windows
    /// WASAPI exclusive mode). Ignored by backends without [`Capabilities::exclusive_mode`].
    pub exclusive: bool,
}

/// A device the backend creates for a virtual strip or bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDevice {
    /// Stable system name, e.g. `openmeeter_v1`.
    pub name: String,
    /// What apps show in their device lists, e.g. "OpenMeeter Virtual Input 1".
    pub description: String,
}

/// A running device stream, as shown in System Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    /// The device's own format (the engine converts to 48 kHz stereo float).
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    /// Stream buffer size in engine frames.
    pub buffer_frames: u32,
    /// The stream bypasses the OS mixer (Windows exclusive mode).
    pub exclusive: bool,
}

/// The complete desired routing state. Backends reconcile towards it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoutingGraph {
    pub sources: Vec<GraphNode>,
    pub sinks: Vec<GraphNode>,
    /// `(source key, sink key)` pairs that should carry audio.
    pub routes: Vec<(String, String)>,
}

/// Peak levels (linear, 0.0..=1.0) per channel, keyed by node key.
pub type Levels = std::collections::HashMap<String, Vec<f32>>;

pub trait AudioBackend: Send {
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// Enumerate the audio endpoints currently present.
    fn devices(&mut self) -> Result<Vec<DeviceInfo>>;

    /// Make the system match `graph`. Called whenever the mixer changes;
    /// implementations should diff against what they already applied.
    fn apply(&mut self, graph: &RoutingGraph) -> Result<()>;

    /// Latest meter levels. Cheap; called every UI frame.
    fn levels(&self) -> Levels {
        Levels::new()
    }

    /// Nodes (by key) whose device failed to start or stopped, with the reason.
    fn node_errors(&self) -> std::collections::HashMap<String, String> {
        Default::default()
    }

    /// Stop and reopen every audio stream, e.g. after a device came back. Takes
    /// effect on the next `apply`.
    fn restart_engine(&mut self) {}

    /// How much audio to buffer per source, in milliseconds: lower is less
    /// latency, higher survives CPU spikes better. Restarts streams if it changes.
    fn set_buffer_ms(&mut self, ms: u32) {
        let _ = ms;
    }

    /// The recorder's file player. The graph routes it with the source key
    /// [`PLAYER_KEY`]; `None` if this backend can't play files yet.
    fn player(&self) -> Option<Arc<Player>> {
        None
    }

    /// Start capturing the nodes in `keys`: input strips are tapped before their
    /// faders, buses after theirs. Nodes without a working device are skipped;
    /// errors if none can be recorded.
    fn start_recording(&mut self, keys: &[String]) -> Result<Box<dyn RecordingStream>> {
        let _ = keys;
        Err(BackendError::NotImplemented("recording"))
    }

    /// Stop feeding the current recording stream.
    fn stop_recording(&mut self) {}

    /// Running streams by node key (missing = not running).
    fn stream_info(&self) -> std::collections::HashMap<String, StreamInfo> {
        Default::default()
    }

    /// Engine health counters for debugging (underruns, dropped audio, ...).
    fn stats(&self) -> Vec<String> {
        Vec::new()
    }

    /// Environment problems the user should fix (e.g. VB-Cable missing).
    fn diagnostics(&mut self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, direction: Direction) -> DeviceInfo {
        let id = DeviceId(format!("{name}/{direction:?}"));
        DeviceInfo { id, name: name.into(), direction, channels: 2, is_virtual: name.contains("VB-Audio"), is_default: false }
    }

    fn devices() -> Vec<DeviceInfo> {
        vec![
            dev("CABLE Output (VB-Audio Virtual Cable)", Direction::Capture),
            dev("CABLE In 16ch (VB-Audio Virtual Cable)", Direction::Playback),
            dev("CABLE Input (VB-Audio Virtual Cable)", Direction::Playback),
            dev("CABLE-A Output (VB-Audio Cable A)", Direction::Capture),
            dev("CABLE-A Input (VB-Audio Cable A)", Direction::Playback),
            dev("Speakers (Realtek(R) Audio)", Direction::Playback),
        ]
    }

    #[test]
    fn cables_pair_by_driver_and_prefer_the_stereo_endpoint() {
        let d = devices();
        assert_eq!(cable_partner(&d, &d[0]).unwrap().name, "CABLE Input (VB-Audio Virtual Cable)");
        assert_eq!(cable_partner(&d, &d[1]).unwrap().name, "CABLE Output (VB-Audio Virtual Cable)");
        assert_eq!(cable_partner(&d, &d[3]).unwrap().name, "CABLE-A Input (VB-Audio Cable A)");
        assert!(cable_partner(&d, &d[5]).is_none(), "hardware has no partner");
    }

    #[test]
    fn resolve_swaps_virtual_sides_but_not_hardware() {
        let d = devices();
        // Virtual input named "CABLE Input": the engine records from CABLE Output.
        assert_eq!(resolve_endpoint(&d, &d[2].id, Direction::Capture).unwrap().id, d[0].id);
        // B bus named "CABLE Output": the engine plays into CABLE Input.
        assert_eq!(resolve_endpoint(&d, &d[0].id, Direction::Playback).unwrap().id, d[2].id);
        // Already the right side (old configs): used as-is.
        assert_eq!(resolve_endpoint(&d, &d[0].id, Direction::Capture).unwrap().id, d[0].id);
        assert!(resolve_endpoint(&d, &d[5].id, Direction::Capture).is_none());
    }
}
