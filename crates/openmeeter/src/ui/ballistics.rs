//! Meter ballistics: turns raw per-frame peaks into a steady display, like a
//! hardware PPM. Fast attack so transients show, a constant-rate release so the
//! bar falls smoothly instead of flickering, and a peak-hold marker.

use std::collections::HashMap;

use openmeeter_backend::Levels;

use super::widgets::level_fraction;

/// Attack time constant: how quickly the bar rises to a new peak.
const ATTACK_SECS: f32 = 0.015;
/// Release speed in meter heights per second (the meter spans 60 dB, so 0.4 = 24 dB/s).
const RELEASE_PER_SEC: f32 = 0.4;
/// How long the peak-hold marker stays before it starts falling.
const HOLD_SECS: f32 = 1.0;

/// One channel's displayed state, in meter-height fractions (0..1).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeterLevel {
    pub level: f32,
    pub hold: f32,
    hold_age: f32,
}

impl MeterLevel {
    fn update(&mut self, target: f32, dt: f32) {
        if target > self.level {
            let attack = 1.0 - (-dt / ATTACK_SECS).exp();
            self.level += (target - self.level) * attack;
        } else {
            self.level = (self.level - RELEASE_PER_SEC * dt).max(target);
        }

        if self.level >= self.hold {
            self.hold = self.level;
            self.hold_age = 0.0;
        } else {
            self.hold_age += dt;
            if self.hold_age > HOLD_SECS {
                self.hold = (self.hold - RELEASE_PER_SEC * dt).max(self.level);
            }
        }
    }
}

#[derive(Default)]
pub struct Ballistics {
    meters: HashMap<String, Vec<MeterLevel>>,
}

impl Ballistics {
    /// Advance every meter by `dt` seconds towards the latest raw peaks.
    /// Meters missing from `peaks` fall back to silence.
    pub fn update(&mut self, peaks: &Levels, dt: f32) {
        for (key, channels) in peaks {
            let meter = self.meters.entry(key.clone()).or_default();
            meter.resize(channels.len(), MeterLevel::default());
        }
        for (key, meter) in &mut self.meters {
            let raw = peaks.get(key);
            for (i, channel) in meter.iter_mut().enumerate() {
                let peak = raw.and_then(|r| r.get(i)).copied().unwrap_or(0.0);
                channel.update(level_fraction(peak), dt);
            }
        }
    }

    pub fn get(&self, key: &str) -> &[MeterLevel] {
        self.meters.get(key).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f32 = 1.0 / 60.0;

    #[test]
    fn rises_fast_and_falls_at_a_steady_rate() {
        let mut m = MeterLevel::default();
        for _ in 0..3 {
            m.update(1.0, FRAME);
        }
        assert!(m.level > 0.95, "attack reached {}", m.level);

        let before = m.level;
        m.update(0.0, 0.5);
        assert!((m.level - (before - RELEASE_PER_SEC * 0.5)).abs() < 1e-4, "release is rate-limited");
    }

    #[test]
    fn hold_marker_waits_then_falls() {
        let mut m = MeterLevel::default();
        m.update(1.0, 1.0);
        m.update(0.0, HOLD_SECS * 0.5);
        assert_eq!(m.hold, 1.0, "still holding");
        m.update(0.0, HOLD_SECS);
        assert!(m.hold < 1.0, "falls after the hold time");
        assert!(m.hold >= m.level);
    }

    #[test]
    fn missing_meters_decay_to_silence() {
        let mut b = Ballistics::default();
        b.update(&Levels::from([("hw1".to_string(), vec![1.0, 1.0])]), 1.0);
        for _ in 0..10 {
            b.update(&Levels::new(), 1.0);
        }
        assert!(b.get("hw1").iter().all(|c| c.level == 0.0 && c.hold == 0.0));
    }
}
