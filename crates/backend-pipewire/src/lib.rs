//! Linux backend: the shared mixing engine on PipeWire, via its PulseAudio
//! interface (pipewire-pulse). Plain PulseAudio works too.
//!
//! Virtual devices are created on demand as null sinks:
//! - a virtual input strip `openmeeter_v1` is a sink apps play into; the engine
//!   records its monitor;
//! - a virtual bus `openmeeter_B1` is a source apps record from, fed by a hidden
//!   `openmeeter_B1_feed` sink the engine plays into.
#![cfg(target_os = "linux")]

mod pactl;
mod pulse;
mod rt;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openmeeter_backend::engine::{EngineHost, Topology};
use openmeeter_backend::{
    AudioBackend, BackendError, Capabilities, DeviceInfo, GraphNode, Levels, Player, RecordingStream, Result, RoutingGraph, StreamInfo,
};
use pactl::Module;

/// One node the backend creates for a virtual strip or bus: a module to load.
#[derive(Debug, Clone, PartialEq)]
struct Want {
    module: &'static str,
    name: String,
    is_source: bool,
    args: Vec<String>,
}

impl Want {
    fn null_sink(name: String, description: &str) -> Self {
        Self { module: "module-null-sink", args: pactl::null_sink_args(&name, description), name, is_source: false }
    }

    fn remap_source(name: String, description: &str, master: &str) -> Self {
        Self { module: "module-remap-source", args: pactl::remap_source_args(&name, description, master), name, is_source: true }
    }

    /// Whether `module` already provides exactly this node.
    fn matches(&self, module: &Module) -> bool {
        module.node == self.name && module.args == self.args.join(" ")
    }
}

/// The sink the engine plays into for a virtual bus.
fn feed_sink(name: &str) -> String {
    format!("{name}_feed")
}

/// Nodes to create for the virtual strips and buses in `graph`, sinks before the
/// sources that depend on them.
fn wanted(graph: &RoutingGraph) -> Vec<Want> {
    let mut wants = Vec::new();
    for node in &graph.sources {
        if let Some(device) = &node.create_virtual {
            wants.push(Want::null_sink(device.name.clone(), &device.description));
        }
    }
    for node in &graph.sinks {
        if let Some(device) = &node.create_virtual {
            // Only the engine should play into this, but it shows up in output lists.
            let feed = feed_sink(&device.name);
            wants.push(Want::null_sink(feed.clone(), &format!("OpenMeeter {} (internal)", node.key)));
            wants.push(Want::remap_source(device.name.clone(), &device.description, &format!("{feed}.monitor")));
        }
    }
    wants
}

/// The device the engine opens for a node: virtual strips record their sink's
/// monitor and virtual buses play into their feed sink.
fn source_device(node: &GraphNode) -> Option<String> {
    match &node.create_virtual {
        Some(device) => Some(format!("{}.monitor", device.name)),
        None => node.device.as_ref().map(|d| d.0.clone()),
    }
}

fn sink_device(node: &GraphNode) -> Option<String> {
    match &node.create_virtual {
        Some(device) => Some(feed_sink(&device.name)),
        None => node.device.as_ref().map(|d| d.0.clone()),
    }
}

/// Routes that would loop: a source recording the monitor of the very sink it
/// would play into (e.g. "Monitor of Speakers" routed to the Speakers bus).
fn feedback_routes(graph: &RoutingGraph) -> Vec<(String, String)> {
    let mut loops = Vec::new();
    for source in &graph.sources {
        let Some(recorded) = source_device(source) else { continue };
        for sink in &graph.sinks {
            if sink_device(sink).is_some_and(|played| recorded == format!("{played}.monitor")) {
                loops.push((source.key.clone(), sink.key.clone()));
            }
        }
    }
    loops
}

pub struct PipeWireBackend {
    host: EngineHost,
    formats: Arc<Mutex<HashMap<String, StreamInfo>>>,
    /// Modules we loaded (or adopted from a previous run) for virtual devices.
    modules: Vec<Module>,
    /// `(source, sink)` routes that would feed back, found at the last engine start.
    loops: Vec<(String, String)>,
}

impl PipeWireBackend {
    pub fn new() -> Result<Self> {
        let formats = Arc::new(Mutex::new(HashMap::new()));
        let host = EngineHost::new(Arc::new(pulse::Pulse { formats: formats.clone() }));
        Ok(Self { host, formats, modules: Vec::new(), loops: Vec::new() })
    }

    /// Create the virtual nodes `graph` needs and remove ours that it doesn't.
    fn sync_virtual_devices(&mut self, graph: &RoutingGraph) -> Result<()> {
        let wants = wanted(graph);
        // Look at what's really loaded: a crashed run may have left nodes behind
        // (possibly with outdated settings), and modules can be unloaded behind our back.
        let (mut loaded, unwanted) = pactl::own_modules()?.into_iter().partition::<Vec<_>, _>(|m| wants.iter().any(|w| w.matches(m)));
        unload_all(unwanted);

        let mut errors = Vec::new();
        for want in &wants {
            if loaded.iter().any(|m| want.matches(m)) {
                continue;
            }
            match pactl::load(want.module, &want.name, want.is_source, &want.args) {
                Ok(module) => loaded.push(module),
                Err(e) => errors.push(e.to_string()),
            }
        }
        self.modules = loaded;
        if errors.is_empty() { Ok(()) } else { Err(BackendError::Platform(errors.join("; "))) }
    }

    fn restart(&mut self, graph: &RoutingGraph, topology: Topology) -> Result<()> {
        // Stop first: streams on a removed node would be moved to the default device.
        self.host.stop();
        let created = self.sync_virtual_devices(graph);
        match pactl::devices() {
            Ok((_, formats)) => {
                if let Ok(mut f) = self.formats.lock() {
                    *f = formats;
                }
            }
            Err(e) => log::warn!("could not read device formats: {e}"),
        }
        self.loops = feedback_routes(graph);
        let sources = graph.sources.iter().map(source_device).collect();
        let sinks = graph.sinks.iter().map(sink_device).collect();
        self.host.start(topology, sources, sinks, Vec::new());
        created
    }
}

/// Unload modules, sources first so none outlives the sink it reads.
fn unload_all(mut modules: Vec<Module>) {
    modules.sort_by_key(|m| !m.is_source);
    for module in &modules {
        pactl::unload(module);
    }
}

/// Remove every virtual device OpenMeeter created, e.g. from a signal handler
/// where the backend can't be dropped normally.
pub fn remove_virtual_devices() {
    match pactl::own_modules() {
        Ok(modules) => unload_all(modules),
        Err(e) => log::warn!("could not remove virtual devices: {e}"),
    }
}

impl Drop for PipeWireBackend {
    fn drop(&mut self) {
        self.host.stop();
        unload_all(std::mem::take(&mut self.modules));
    }
}

impl AudioBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "pipewire"
    }

    fn capabilities(&self) -> Capabilities {
        // Per-app routing (moving app streams between sinks) is TODO.
        Capabilities { create_virtual_devices: true, per_app_routing: false, exclusive_mode: false }
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>> {
        Ok(pactl::devices()?.0)
    }

    fn apply(&mut self, graph: &RoutingGraph) -> Result<()> {
        let topology = Topology::of(graph);
        let created = if self.host.needs_start(&topology) { self.restart(graph, topology) } else { Ok(()) };

        let mut graph = graph.clone();
        graph.routes.retain(|route| !self.loops.contains(route));
        let controls = self.host.set_controls(&graph);

        let mut problems: Vec<String> = [created, controls].into_iter().filter_map(|r| r.err().map(|e| e.to_string())).collect();
        for (source, sink) in &self.loops {
            problems.push(format!("{source} can't feed {sink}: it records {sink}'s own output, which would loop"));
        }
        if problems.is_empty() { Ok(()) } else { Err(BackendError::Platform(problems.join("; "))) }
    }

    fn levels(&self) -> Levels {
        self.host.levels()
    }

    fn restart_engine(&mut self) {
        self.host.stop();
    }

    fn set_buffer_ms(&mut self, ms: u32) {
        self.host.set_buffer_ms(ms);
    }

    fn player(&self) -> Option<Arc<Player>> {
        Some(self.host.player())
    }

    fn start_recording(&mut self, keys: &[String]) -> Result<Box<dyn RecordingStream>> {
        self.host.start_recording(keys)
    }

    fn stop_recording(&mut self) {
        self.host.stop_recording();
    }

    fn node_errors(&self) -> HashMap<String, String> {
        self.host.node_errors()
    }

    fn stream_info(&self) -> HashMap<String, StreamInfo> {
        self.host.stream_info()
    }

    fn stats(&self) -> Vec<String> {
        let mut stats = self.host.stats();
        stats.extend(rt::summary().map(String::from));
        stats
    }

    fn diagnostics(&mut self) -> Vec<String> {
        match pactl::server_name() {
            Ok(name) => {
                log::info!("sound server: {name}");
                Vec::new()
            }
            Err(e) => vec![format!("Can't reach the sound server: {e}")],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmeeter_backend::{DeviceId, VirtualDevice};

    fn node(key: &str, device: Option<&str>, create_virtual: Option<&str>) -> GraphNode {
        GraphNode {
            key: key.into(),
            device: device.map(|d| DeviceId(d.into())),
            create_virtual: create_virtual.map(|name| VirtualDevice { name: name.into(), description: format!("OpenMeeter {key}") }),
            gain_db: 0.0,
            mute: false,
            mono: false,
            reverse: false,
            delay_ms: 0.0,
            exclusive: false,
        }
    }

    fn graph() -> RoutingGraph {
        RoutingGraph {
            sources: vec![node("hw1", Some("alsa_output.speakers.monitor"), None), node("v1", None, Some("openmeeter_v1"))],
            sinks: vec![node("A1", Some("alsa_output.speakers"), None), node("B1", None, Some("openmeeter_B1"))],
            routes: vec![],
        }
    }

    #[test]
    fn virtual_nodes_map_to_null_sinks() {
        let g = graph();
        assert_eq!(source_device(&g.sources[1]).as_deref(), Some("openmeeter_v1.monitor"));
        assert_eq!(sink_device(&g.sinks[1]).as_deref(), Some("openmeeter_B1_feed"));
        let wants = wanted(&g);
        let names: Vec<_> = wants.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["openmeeter_v1", "openmeeter_B1_feed", "openmeeter_B1"], "feed sink before its source");
        assert!(wants[2].args.contains(&"source_properties=\"device.description='OpenMeeter B1'\"".to_string()));
        let loaded = Module { id: 7, node: "openmeeter_v1".into(), is_source: false, args: wants[0].args.join(" ") };
        assert!(wants[0].matches(&loaded));
        let outdated = Module { args: loaded.args.replace("OpenMeeter v1", "OpenMeeter"), ..loaded };
        assert!(!wants[0].matches(&outdated), "a node with an old description is recreated");
    }

    #[test]
    fn recording_a_sinks_monitor_into_that_sink_is_a_loop() {
        assert_eq!(feedback_routes(&graph()), [("hw1".to_string(), "A1".to_string())]);
    }
}
