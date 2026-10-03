//! Windows backend: WASAPI device access plus our own mixing engine.
//! Virtual endpoints come from VB-Cable, which must be installed.
#![cfg(windows)]

mod engine;

use std::sync::Arc;

use openmeeter_backend::engine::{EngineHost, Topology};
use openmeeter_backend::{
    driver_name, resolve_endpoint, Player, RecordingStream, AudioBackend, BackendError, Capabilities, DeviceId, DeviceInfo, Direction, Levels, Result, RoutingGraph,
};

/// Name fragments of virtual-cable drivers: any VB-Audio driver (VB-Cable, its
/// A+B/C+D add-ons, Hi-Fi Cable, Voicemeeter's VAIO) and Muzychenko's Virtual Audio Cable.
const VIRTUAL_MARKERS: [&str; 2] = ["VB-Audio", "Virtual Audio Cable"];

fn is_virtual_name(name: &str) -> bool {
    VIRTUAL_MARKERS.iter().any(|m| name.contains(m))
}
/// VB-Cable specifically: "CABLE Input (VB-Audio Virtual Cable)". Voicemeeter's
/// VAIO endpoints are tied to the Voicemeeter app, so they don't count.
const VB_CABLE_MARKER: &str = "VB-Audio Virtual Cable";

pub struct WindowsBackend {
    host: EngineHost,
}

impl WindowsBackend {
    pub fn new() -> Result<Self> {
        // COM is initialised lazily per call: the GUI thread must stay single-threaded
        // (winit calls OleInitialize), so we can't claim it for the MTA up front.
        Ok(Self { host: EngineHost::new(Arc::new(engine::Wasapi)) })
    }

    fn restart(&mut self, topology: Topology) -> Result<()> {
        // Stop the old engine first so its devices are released.
        self.host.stop();
        let devices = self.devices()?;
        // Sources record, sinks play; virtual devices are named by the side apps use,
        // so swap each cable for the side the engine needs.
        let resolve = |nodes: &[(String, Option<DeviceId>)], direction| {
            nodes
                .iter()
                .map(|(_, id)| id.as_ref().and_then(|id| resolve_endpoint(&devices, id, direction)))
                .collect::<Vec<_>>()
        };
        let sources = resolve(&topology.sources, Direction::Capture);
        let sinks = resolve(&topology.sinks, Direction::Playback);

        // A VB-Cable is one loop: whatever a B bus plays into it, a virtual input on the
        // same cable records again (you'd hear your own mic). The B bus wins; the virtual
        // input is disabled and reports why.
        let mut disabled: Vec<(usize, String)> = Vec::new();
        for (s, src) in sources.iter().enumerate() {
            let clash = sinks.iter().enumerate().find_map(|(k, sink)| {
                let sink = (*sink)?;
                is_feedback((*src)?, sink).then_some(k)
            });
            if let Some(k) = clash {
                let cable = src.and_then(|d| driver_name(&d.name)).unwrap_or("this cable");
                disabled.push((
                    s,
                    format!(
                        "Disabled: {} already uses {cable}. One cable can be a virtual input or a B bus, not both; add another cable to use both.",
                        topology.sinks[k].0
                    ),
                ));
            }
        }
        // An unresolvable binding (device unplugged) is passed through so the engine reports it.
        let ids = |nodes: &[(String, Option<DeviceId>)], resolved: &[Option<&DeviceInfo>]| {
            nodes
                .iter()
                .zip(resolved)
                .map(|((_, id), r)| r.map(|d| d.id.0.clone()).or_else(|| id.as_ref().map(|id| id.0.clone())))
                .collect::<Vec<_>>()
        };
        let (source_ids, sink_ids) = (ids(&topology.sources, &sources), ids(&topology.sinks, &sinks));
        self.host.start(topology, source_ids, sink_ids, disabled);
        Ok(())
    }
}

/// Capturing a virtual cable's output and playing into the same cable's input
/// would loop audio forever.
fn is_feedback(source: &DeviceInfo, sink: &DeviceInfo) -> bool {
    source.is_virtual && sink.is_virtual && driver_name(&source.name).is_some() && driver_name(&source.name) == driver_name(&sink.name)
}

/// Make sure COM is initialised on this thread. If the thread already joined a
/// single-threaded apartment (the winit GUI thread), that works for WASAPI too.
fn ensure_com() -> Result<()> {
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106_u32 as i32;
    let hr = wasapi::initialize_mta();
    if hr.is_err() && hr.0 != RPC_E_CHANGED_MODE {
        return Err(BackendError::Platform(format!("COM init failed: {hr:?}")));
    }
    Ok(())
}

fn platform_err(e: impl std::fmt::Display) -> BackendError {
    BackendError::Platform(e.to_string())
}

fn enumerate(direction: Direction) -> Result<Vec<DeviceInfo>> {
    ensure_com()?;
    let wasapi_dir = match direction {
        Direction::Capture => wasapi::Direction::Capture,
        Direction::Playback => wasapi::Direction::Render,
    };
    let enumerator = wasapi::DeviceEnumerator::new().map_err(platform_err)?;
    let collection = enumerator.get_device_collection(&wasapi_dir).map_err(platform_err)?;
    let default_id = enumerator.get_default_device(&wasapi_dir).and_then(|d| d.get_id()).ok();

    let mut devices = Vec::new();
    for device in &collection {
        let device = device.map_err(platform_err)?;
        let name = device.get_friendlyname().map_err(platform_err)?;
        let id = device.get_id().map_err(platform_err)?;
        let channels = device
            .get_iaudioclient()
            .and_then(|c| c.get_mixformat())
            .map(|f| f.get_nchannels())
            .unwrap_or(2);
        devices.push(DeviceInfo {
            is_virtual: is_virtual_name(&name),
            is_default: default_id.as_deref() == Some(id.as_str()),
            id: DeviceId(id),
            name,
            direction,
            channels,
        });
    }
    Ok(devices)
}

impl AudioBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "wasapi"
    }

    fn capabilities(&self) -> Capabilities {
        // Virtual devices come from VB-Cable; per-app routing (IAudioPolicyConfigFactory) is TODO.
        Capabilities { create_virtual_devices: false, per_app_routing: false, exclusive_mode: true }
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>> {
        let mut devices = enumerate(Direction::Capture)?;
        devices.extend(enumerate(Direction::Playback)?);
        Ok(devices)
    }

    fn apply(&mut self, graph: &RoutingGraph) -> Result<()> {
        let topology = Topology::of(graph);
        if self.host.needs_start(&topology) {
            self.restart(topology)?;
        }
        self.host.set_controls(graph)
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

    fn node_errors(&self) -> std::collections::HashMap<String, String> {
        self.host.node_errors()
    }

    fn stream_info(&self) -> std::collections::HashMap<String, openmeeter_backend::StreamInfo> {
        self.host.stream_info()
    }

    fn stats(&self) -> Vec<String> {
        self.host.stats()
    }

    fn diagnostics(&mut self) -> Vec<String> {
        match self.devices() {
            Ok(devices) if devices.iter().any(|d| d.name.contains(VB_CABLE_MARKER)) => Vec::new(),
            Ok(_) => vec!["VB-Cable not found. Install it from https://vb-audio.com/Cable/ to use virtual inputs.".into()],
            Err(e) => vec![format!("Could not enumerate audio devices: {e}")],
        }
    }
}

/// An app's audio session on a playback device, for diagnostics (`openmeeter apps`).
pub struct AppSession {
    pub device: String,
    pub pid: u32,
    pub process: String,
    pub active: bool,
}

/// Every non-expired audio session on every active playback and recording device.
pub fn app_sessions() -> Result<Vec<AppSession>> {
    ensure_com()?;
    let enumerator = wasapi::DeviceEnumerator::new().map_err(platform_err)?;
    let mut sessions = Vec::new();
    for (direction, side) in [(wasapi::Direction::Render, "playback"), (wasapi::Direction::Capture, "recording")] {
        let collection = enumerator.get_device_collection(&direction).map_err(platform_err)?;
        collect_sessions(&collection, side, &mut sessions)?;
    }
    Ok(sessions)
}

fn collect_sessions(collection: &wasapi::DeviceCollection, side: &str, sessions: &mut Vec<AppSession>) -> Result<()> {
    for device in collection {
        let device = device.map_err(platform_err)?;
        let name = format!("{} [{side}]", device.get_friendlyname().unwrap_or_default());
        let list = device.get_iaudiosessionmanager().and_then(|m| m.get_audiosessionenumerator()).map_err(platform_err)?;
        for i in 0..list.get_count().map_err(platform_err)? {
            let Ok(session) = list.get_session(i) else { continue };
            let state = session.get_state().map_err(platform_err)?;
            if matches!(state, wasapi::SessionState::Expired) {
                continue;
            }
            let pid = session.get_process_id().unwrap_or(0);
            sessions.push(AppSession { device: name.clone(), pid, process: process_name(pid), active: matches!(state, wasapi::SessionState::Active) });
        }
    }
    Ok(())
}

fn process_name(pid: u32) -> String {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    if pid == 0 {
        return "System sounds".into();
    }
    // SAFETY: the handle is only used for the query below and closed by windows-rs on drop.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return format!("pid {pid}") };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if !ok {
            return format!("pid {pid}");
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, direction: Direction) -> DeviceInfo {
        DeviceInfo { id: DeviceId(name.into()), name: name.into(), direction, channels: 2, is_virtual: is_virtual_name(name), is_default: false }
    }

    #[test]
    fn driver_name_is_the_parenthesised_suffix() {
        assert_eq!(driver_name("CABLE Output (VB-Audio Virtual Cable)"), Some("VB-Audio Virtual Cable"));
        assert_eq!(driver_name("Speakers"), None);
    }

    #[test]
    fn same_cable_in_and_out_is_feedback() {
        let out = dev("CABLE Output (VB-Audio Virtual Cable)", Direction::Capture);
        let input = dev("CABLE Input (VB-Audio Virtual Cable)", Direction::Playback);
        let in16 = dev("CABLE In 16ch (VB-Audio Virtual Cable)", Direction::Playback);
        let vaio = dev("VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)", Direction::Playback);
        let vac_out = dev("Line 1 (Virtual Audio Cable)", Direction::Capture);
        let vac_in = dev("Line 1 (Virtual Audio Cable)", Direction::Playback);
        assert!(vac_out.is_virtual, "VAC counts as a virtual cable");
        assert!(is_feedback(&vac_out, &vac_in));
        let speakers = dev("Speakers (Realtek(R) Audio)", Direction::Playback);
        assert!(is_feedback(&out, &input));
        assert!(is_feedback(&out, &in16));
        assert!(!is_feedback(&out, &vaio));
        assert!(!is_feedback(&out, &speakers));
    }
}
