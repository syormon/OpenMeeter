use crate::{AudioBackend, Capabilities, DeviceId, DeviceInfo, Direction, Levels, Result, RoutingGraph};
use std::sync::atomic::{AtomicU32, Ordering};

/// Fake backend for UI development and tests; available on every OS.
#[derive(Default)]
pub struct MockBackend {
    applied: RoutingGraph,
    frame: AtomicU32,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

fn device(id: &str, name: &str, direction: Direction, is_virtual: bool) -> DeviceInfo {
    DeviceInfo { id: DeviceId(id.into()), name: name.into(), direction, channels: 2, is_virtual, is_default: false }
}

impl AudioBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { create_virtual_devices: true, per_app_routing: true }
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>> {
        Ok(vec![
            device("mic", "USB Microphone", Direction::Capture, false),
            device("line", "Line In", Direction::Capture, false),
            device("cable-out", "CABLE Output (VB-Audio Virtual Cable)", Direction::Capture, true),
            device("speakers", "Speakers", Direction::Playback, false),
            device("headphones", "Headphones", Direction::Playback, false),
            device("cable-in", "CABLE Input (VB-Audio Virtual Cable)", Direction::Playback, true),
        ])
    }

    fn apply(&mut self, graph: &RoutingGraph) -> Result<()> {
        if *graph != self.applied {
            log::debug!("mock apply: {} routes", graph.routes.len());
            self.applied = graph.clone();
        }
        Ok(())
    }

    fn levels(&self) -> Levels {
        // Animated fake peaks so meters can be developed without real audio.
        let t = self.frame.fetch_add(1, Ordering::Relaxed);
        let keys = self.applied.sources.iter().map(|s| (&s.key, s.mute, s.gain_db));
        let keys = keys.chain(self.applied.sinks.iter().map(|s| (&s.key, s.mute, s.gain_db)));
        keys.enumerate()
            .map(|(i, (key, mute, gain_db))| {
                let base = if mute { 0.0 } else { 10f32.powf(gain_db / 20.0) * 0.6 };
                let wobble = |c: f32| (((t as f32) * 0.07 + i as f32 * 1.3 + c).sin() * 0.5 + 0.5) * base;
                (key.clone(), vec![wobble(0.0).min(1.0), wobble(0.8).min(1.0)])
            })
            .collect()
    }
}
