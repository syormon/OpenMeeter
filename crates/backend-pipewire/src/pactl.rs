//! The control side: listing devices and creating virtual ones through `pactl`,
//! which talks to pipewire-pulse (or plain PulseAudio) for us.

use std::collections::HashMap;
use std::process::Command;

use openmeeter_backend::{BackendError, DeviceId, DeviceInfo, Direction, Result, StreamInfo};
use serde::Deserialize;

/// Prefix of every node we create; they're managed by the mixer, not offered as devices.
pub const OWN_PREFIX: &str = "openmeeter_";

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl").args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            BackendError::Platform("pactl not found; install it with: sudo apt install pulseaudio-utils".into())
        } else {
            BackendError::Platform(format!("pactl: {e}"))
        }
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(BackendError::Platform(format!("pactl {}: {}", args.join(" "), err.trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T> {
    let mut full = vec!["-f", "json"];
    full.extend_from_slice(args);
    serde_json::from_str(&pactl(&full)?).map_err(|e| BackendError::Platform(format!("pactl {}: unexpected output: {e}", args.join(" "))))
}

#[derive(Deserialize)]
struct ServerInfo {
    #[serde(default)]
    server_name: String,
    #[serde(default)]
    default_sink_name: String,
    #[serde(default)]
    default_source_name: String,
}

#[derive(Deserialize)]
struct Node {
    name: String,
    description: String,
    sample_specification: String,
    #[serde(default)]
    properties: HashMap<String, serde_json::Value>,
    #[serde(default)]
    active_port: Option<String>,
    #[serde(default)]
    ports: Vec<Port>,
}

#[derive(Deserialize)]
struct Port {
    name: String,
    description: String,
}

impl Node {
    fn property(&self, key: &str) -> Option<&str> {
        self.properties.get(key)?.as_str()
    }

    /// The name desktop sound settings show, e.g. "Internal Microphone - Built-in
    /// Audio": the active port, then the card. Falls back to the node's description.
    fn display_name(&self) -> String {
        let port = self.active_port.as_ref().and_then(|active| self.ports.iter().find(|p| &p.name == active));
        match (port, self.property("device.description")) {
            (Some(port), Some(card)) if self.property("device.class") == Some("monitor") => format!("Monitor of {} - {card}", port.description),
            (Some(port), Some(card)) => format!("{} - {card}", port.description),
            _ => self.description.clone(),
        }
    }
}

/// The sound server's name, e.g. "PulseAudio (on PipeWire 1.0.5)".
pub fn server_name() -> Result<String> {
    Ok(json::<ServerInfo>(&["info"])?.server_name)
}

/// A device's native format from its sample spec, e.g. "s32le 2ch 48000Hz".
pub fn parse_spec(spec: &str) -> Option<StreamInfo> {
    let mut parts = spec.split_whitespace();
    let format = parts.next()?;
    let channels = parts.next()?.strip_suffix("ch")?.parse().ok()?;
    let sample_rate = parts.next()?.strip_suffix("Hz")?.parse().ok()?;
    let bits = match format {
        f if f.starts_with("u8") || f.starts_with("alaw") || f.starts_with("ulaw") => 8,
        f if f.starts_with("s16") => 16,
        f if f.starts_with("s24") => 24,
        _ => 32,
    };
    Some(StreamInfo { sample_rate, channels, bits, buffer_frames: 0, exclusive: false })
}

/// Every sink (playback) and source (capture) except our own, plus each one's
/// native format by name.
pub fn devices() -> Result<(Vec<DeviceInfo>, HashMap<String, StreamInfo>)> {
    let info: ServerInfo = json(&["info"])?;
    let mut devices = Vec::new();
    let mut formats = HashMap::new();
    for (kind, direction, default) in
        [("sources", Direction::Capture, &info.default_source_name), ("sinks", Direction::Playback, &info.default_sink_name)]
    {
        for node in json::<Vec<Node>>(&["list", kind])? {
            let spec = parse_spec(&node.sample_specification);
            if let Some(spec) = spec {
                formats.insert(node.name.clone(), spec);
            }
            if node.name.starts_with(OWN_PREFIX) {
                continue;
            }
            devices.push(DeviceInfo {
                // Monitors carry their sink's properties, so a null sink's monitor is virtual too.
                is_virtual: node.property("factory.name") == Some("support.null-audio-sink"),
                is_default: &node.name == default,
                channels: spec.map_or(2, |s| s.channels),
                name: node.display_name(),
                id: DeviceId(node.name),
                direction,
            });
        }
    }
    Ok((devices, formats))
}

/// A loaded module that creates one of our nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub id: u32,
    /// The `sink_name` or `source_name` it creates.
    pub node: String,
    /// Creates a source (which may read from one of our sinks) rather than a sink.
    pub is_source: bool,
    /// Its arguments, as loaded.
    pub args: String,
}

/// Our modules that are loaded right now, e.g. left over from a crash.
pub fn own_modules() -> Result<Vec<Module>> {
    Ok(parse_modules(&pactl(&["list", "modules", "short"])?))
}

/// Parse `pactl list modules short`: "id<TAB>name<TAB>arguments".
fn parse_modules(listing: &str) -> Vec<Module> {
    listing
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let id = cols.next()?.trim().parse().ok()?;
            let _name = cols.next()?;
            let args = cols.next()?;
            let (node, is_source) = args.split_whitespace().find_map(|arg| match arg.strip_prefix("sink_name=") {
                Some(sink) => Some((sink, false)),
                None => arg.strip_prefix("source_name=").map(|source| (source, true)),
            })?;
            node.starts_with(OWN_PREFIX).then(|| Module { id, node: node.to_string(), is_source, args: args.trim().to_string() })
        })
        .collect()
}

/// Arguments for a null sink: apps (or the engine) play into it, its monitor records it.
/// The outer quotes keep a description with spaces in one piece.
pub fn null_sink_args(name: &str, description: &str) -> Vec<String> {
    vec![
        format!("sink_name={name}"),
        format!("sink_properties=\"device.description='{description}'\""),
        "rate=48000".into(),
        "channels=2".into(),
        "channel_map=front-left,front-right".into(),
    ]
}

/// Arguments for a source that apps can record from, fed by `master` (e.g. a sink monitor).
pub fn remap_source_args(name: &str, description: &str, master: &str) -> Vec<String> {
    vec![
        format!("source_name={name}"),
        format!("master={master}"),
        format!("source_properties=\"device.description='{description}'\""),
        "channels=2".into(),
        "channel_map=front-left,front-right".into(),
    ]
}

/// Load `module` (e.g. "module-null-sink") creating `node`.
pub fn load(module: &str, node: &str, is_source: bool, args: &[String]) -> Result<Module> {
    let mut full = vec!["load-module", module];
    full.extend(args.iter().map(String::as_str));
    let id = pactl(&full)?.trim().parse().map_err(|_| BackendError::Platform(format!("could not create {node}")))?;
    log::info!("created {node} (module {id})");
    Ok(Module { id, node: node.to_string(), is_source, args: args.join(" ") })
}

pub fn unload(module: &Module) {
    match pactl(&["unload-module", &module.id.to_string()]) {
        Ok(_) => log::info!("removed {} (module {})", module.node, module.id),
        Err(e) => log::warn!("could not remove {}: {e}", module.node),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_into_stream_info() {
        let s = parse_spec("s32le 2ch 48000Hz").unwrap();
        assert_eq!((s.sample_rate, s.channels, s.bits), (48000, 2, 32));
        let s = parse_spec("s16le 1ch 44100Hz").unwrap();
        assert_eq!((s.sample_rate, s.channels, s.bits), (44100, 1, 16));
        assert!(parse_spec("garbage").is_none());
    }

    #[test]
    fn only_our_modules_are_listed() {
        let listing = "1\tlibpipewire-module-rt\t{\n            nice.level    = -11\n}\n\
                       536870916\tmodule-null-sink\tsink_name=openmeeter_v1 sink_properties=device.description='x'\t\n\
                       536870917\tmodule-remap-source\tmaster=openmeeter_B1_feed.monitor source_name=openmeeter_B1\t\n\
                       536870918\tmodule-null-sink\tsink_name=someone_else\t\n";
        assert_eq!(
            parse_modules(listing),
            [
                Module {
                    id: 536870916,
                    node: "openmeeter_v1".into(),
                    is_source: false,
                    args: "sink_name=openmeeter_v1 sink_properties=device.description='x'".into()
                },
                Module {
                    id: 536870917,
                    node: "openmeeter_B1".into(),
                    is_source: true,
                    args: "master=openmeeter_B1_feed.monitor source_name=openmeeter_B1".into()
                },
            ]
        );
    }
}
