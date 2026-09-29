//! The recorder's file player, shared between the app (which loads files and
//! drives the transport) and a backend (which pulls audio into the mix).
//!
//! Tracks are decoded up front to the engine format (48 kHz, stereo,
//! interleaved f32), so the audio path only copies samples.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const PLAYER_KEY: &str = "player";
pub const ENGINE_SAMPLE_RATE: u32 = 48_000;
pub const ENGINE_CHANNELS: usize = 2;

#[derive(Default)]
pub struct Player {
    track: Mutex<Option<Arc<[f32]>>>,
    playing: AtomicBool,
    looping: AtomicBool,
    /// Playback position in frames.
    position: AtomicU64,
}

impl Player {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the loaded track (48 kHz stereo interleaved) and rewind.
    pub fn load(&self, samples: Arc<[f32]>) {
        self.playing.store(false, Ordering::Relaxed);
        self.position.store(0, Ordering::Relaxed);
        if let Ok(mut track) = self.track.lock() {
            *track = Some(samples);
        }
    }

    pub fn unload(&self) {
        self.playing.store(false, Ordering::Relaxed);
        self.position.store(0, Ordering::Relaxed);
        if let Ok(mut track) = self.track.lock() {
            *track = None;
        }
    }

    pub fn play(&self) {
        if self.length() > 0 {
            if self.position() >= self.length() {
                self.seek(0);
            }
            self.playing.store(true, Ordering::Relaxed);
        }
    }

    pub fn pause(&self) {
        self.playing.store(false, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.playing.store(false, Ordering::Relaxed);
        self.position.store(0, Ordering::Relaxed);
    }

    pub fn seek(&self, frame: u64) {
        self.position.store(frame.min(self.length()), Ordering::Relaxed);
    }

    pub fn set_looping(&self, on: bool) {
        self.looping.store(on, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    /// Track length in frames (0 when nothing is loaded).
    pub fn length(&self) -> u64 {
        self.track.lock().ok().and_then(|t| t.as_ref().map(|s| (s.len() / ENGINE_CHANNELS) as u64)).unwrap_or(0)
    }

    /// Fill `out` (interleaved stereo) with the next audio and advance. Returns the
    /// number of frames produced: 0 when paused or nothing is loaded. Reaching the
    /// end stops playback, or wraps to the start when looping. Called from a
    /// backend feeder thread, never the UI.
    pub fn read(&self, out: &mut [f32]) -> usize {
        if !self.is_playing() {
            return 0;
        }
        let Ok(track) = self.track.lock() else { return 0 };
        let Some(samples) = track.as_ref() else { return 0 };
        let total = samples.len() / ENGINE_CHANNELS;
        if total == 0 {
            return 0;
        }
        let wanted = out.len() / ENGINE_CHANNELS;
        let mut pos = (self.position() as usize).min(total);
        let mut done = 0;
        while done < wanted {
            if pos >= total {
                if !self.looping.load(Ordering::Relaxed) {
                    self.playing.store(false, Ordering::Relaxed);
                    break;
                }
                pos = 0;
            }
            let frames = (wanted - done).min(total - pos);
            let (src, dst) = (pos * ENGINE_CHANNELS, done * ENGINE_CHANNELS);
            out[dst..dst + frames * ENGINE_CHANNELS].copy_from_slice(&samples[src..src + frames * ENGINE_CHANNELS]);
            pos += frames;
            done += frames;
        }
        if pos >= total && !self.looping.load(Ordering::Relaxed) {
            self.playing.store(false, Ordering::Relaxed);
        }
        self.position.store(pos as u64, Ordering::Relaxed);
        done
    }
}

/// Audio being recorded from the armed inputs or buses, in the engine format.
/// Read it from a non-realtime thread, e.g. a file writer.
pub trait RecordingStream: Send {
    /// Append the sum of all armed sources that is ready to `out`. Returns the frames appended.
    fn read(&mut self, out: &mut Vec<f32>) -> usize;

    /// Like `read`, but keeps each armed source separate (multitrack): returns
    /// `(node key, samples)` per source, all the same length.
    fn read_tracks(&mut self) -> Vec<(String, Vec<f32>)>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_to_the_end_then_stops() {
        let p = Player::new();
        p.load((0..10).map(|i| i as f32).collect::<Vec<_>>().into()); // 5 frames
        let mut out = [0.0; 6];
        assert_eq!(p.read(&mut out), 0, "paused after load");

        p.play();
        assert_eq!(p.read(&mut out), 3);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(p.read(&mut out), 2);
        assert_eq!(&out[..4], &[6.0, 7.0, 8.0, 9.0]);
        assert!(!p.is_playing(), "stops at the end");

        p.play();
        assert_eq!(p.position(), 0, "play after the end restarts");
    }

    #[test]
    fn looping_wraps_to_the_start() {
        let p = Player::new();
        p.load((0..6).map(|i| i as f32).collect::<Vec<_>>().into()); // 3 frames
        p.set_looping(true);
        p.play();
        let mut out = [0.0; 10]; // 5 frames
        assert_eq!(p.read(&mut out), 5);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 0.0, 1.0, 2.0, 3.0]);
        assert!(p.is_playing());
        assert_eq!(p.position(), 2);
    }

    #[test]
    fn seek_is_clamped_to_the_track() {
        let p = Player::new();
        p.load(vec![0.0; 20].into());
        p.seek(100);
        assert_eq!(p.position(), 10);
    }
}
