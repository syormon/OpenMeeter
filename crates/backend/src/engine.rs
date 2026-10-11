//! Real-time mixing engine, shared by the platform backends.
//!
//! One capture thread per bound source device and one render thread per bound
//! sink device. Every source feeds a lock-free ring buffer per sink; each render
//! thread, paced by its own device clock, mixes the rings routed to it. Every
//! stream runs in the engine format (48 kHz stereo f32): the platform audio API
//! converts, so no sample-rate conversion happens here.
//!
//! Each sink holds some audio from every source in a ring. Three things move a
//! ring's fill level, and each is handled differently:
//!
//! - **Delivery is lumpy.** A source hands over audio in packets, sometimes
//!   several at once (a capture thread woke late, a device with a long period);
//!   a sink asks for blocks of its own size. So the fill swings between a low
//!   point, just before a delivery, and a crest. The engine measures that swing
//!   and regulates the *low point*, holding it at [`Params::target_frames`] (or
//!   what the sink's largest block needs). Lumpy sources thus get exactly the
//!   extra buffering their lumps need; nothing is ever judged by one instant's fill.
//!   A source that still runs dry (an occasional long stall) earns a cushion on top,
//!   which fades again over minutes of clean running.
//! - **Clocks drift.** Devices never share a clock exactly, so each source is
//!   resampled at a rate a few ppm off 1.0, steered by the smoothed low point (see
//!   [`crate::resample`]). No sample is dropped or repeated for drift, which would
//!   be audible as grain on bright material.
//! - **Backlogs appear.** When a sink is held up (a busy system) while a source
//!   keeps delivering, the extra audio would leave that sink playing late for a
//!   minute, out of step with every other output; the rate correction is too
//!   gentle to clear it. A low point that stays well above its target is therefore
//!   skipped in one go, with a short fade either side (see `SNAP_FADE_FRAMES`).
//!
//! Gains, mutes and routes live in [`Params`] as atomics, so changing them never
//! restarts a stream. Device access is the platform's job, through [`DeviceIo`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::resample::DriftResampler;
use crate::{BackendError, ENGINE_CHANNELS, ENGINE_SAMPLE_RATE, GraphNode, Levels, PLAYER_KEY, Player, RecordingStream, Result, RoutingGraph, StreamInfo};

pub const SAMPLE_RATE: usize = ENGINE_SAMPLE_RATE as usize;
pub const CHANNELS: usize = ENGINE_CHANNELS;
/// Default buffered audio per source (20 ms); see [`Params::target_frames`].
pub const DEFAULT_TARGET_FRAMES: usize = 960;
/// Smoothing for a ring's low point (per render callback, ~0.5 s time constant).
const LEVEL_SMOOTHING: f32 = 0.02;
/// A ring's low point and crest are taken over the last one to two of these
/// (0.25 s each): long enough to span several deliveries of the lumpiest source.
const WINDOW_FRAMES: usize = SAMPLE_RATE / 4;
/// Headroom (5 ms) between a ring's low point and what the sink's largest block
/// needs, when that is more than the user's buffer setting.
const MARGIN_FRAMES: usize = 240;
/// Added to a ring's cushion each time it runs dry (at least; a larger block adds
/// more), the most a cushion can reach (100 ms), and how much of it remains after
/// each clean window (so it halves in about three minutes).
const CUSHION_STEP_FRAMES: usize = 240;
const MAX_CUSHION_FRAMES: f32 = 4800.0;
const CUSHION_DECAY: f32 = 0.999;
/// How fast the remembered largest block shrinks (per callback; halves in ~70 s),
/// so one oversized request at start-up doesn't cost latency forever.
const BLOCK_DECAY: f32 = 0.9999;
/// Rate correction per frame of low-point error. With the smoothing this settles
/// in about ten seconds; typical clock drift (under 100 ppm) then holds the low
/// point within 50 frames of its target.
const DRIFT_GAIN: f64 = 2e-6;
/// Largest rate correction (2000 ppm, about 3.5 cents: far beyond real clock
/// drift, so hitting it means the source is bursty rather than drifting).
const MAX_DRIFT: f64 = 2e-3;
/// Backlog beyond a ring's expected crest (130 ms) that is dropped at once, even
/// before the low point is known, e.g. audio queued before the sink started.
const EMERGENCY_EXTRA_FRAMES: usize = 6240;
/// Smallest backlog (10 ms, measured at the low point) that is skipped rather
/// than left to the rate correction; see [`Params::snap_frames`].
const MIN_SNAP_FRAMES: usize = 480;
/// Fade out before and in after a skip, so the cut is a soft dip rather than a click.
const SNAP_FADE_FRAMES: usize = 64;
const RING_FRAMES: usize = 24_000;
/// Longest output delay (Monitoring Synchro Delay): one second.
pub const MAX_DELAY_FRAMES: usize = SAMPLE_RATE;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
/// Two seconds of stereo audio per recorded node.
const RECORD_RING_SAMPLES: usize = SAMPLE_RATE * CHANNELS * 2;

/// `f32` stored as bits so it can be shared lock-free with the audio threads.
#[derive(Default)]
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Live controls and meter output for one source or sink.
#[derive(Default)]
pub struct NodeParams {
    gain: AtomicF32,
    mute: AtomicBool,
    mono: AtomicBool,
    reverse: AtomicBool,
    /// Sinks only: open the device exclusively. Fixed per engine run.
    exclusive: AtomicBool,
    /// Sinks only: output delay in frames (applied after the recording tap).
    delay_frames: AtomicU32,
    /// The device stream, while it runs. Only touched at start/stop and by the UI.
    stream: Mutex<Option<StreamInfo>>,
    /// Per-channel peak since the last `take_peaks`, as `f32` bits. Non-negative
    /// floats order the same as their bit patterns, so `fetch_max` works.
    peaks: [AtomicU32; CHANNELS],
    /// Sinks only: times a source ran dry, and frames discarded to correct drift.
    pub underruns: AtomicU64,
    pub dropped_frames: AtomicU64,
    /// Sinks only: the largest rate correction currently applied to a source, in ppm.
    drift_ppm: AtomicF32,
    /// Sinks only: the low point each ring is held at, the largest swing above it
    /// among the sources, and the largest block the device asked for, in frames.
    floor_frames: AtomicU32,
    swing_frames: AtomicU32,
    max_block: AtomicU32,
    /// Why this node's stream failed to start or stopped (e.g. unplugged).
    /// Only touched on failure and by the UI, never in the audio path.
    error: Mutex<Option<String>>,
    /// While recording, the audio thread copies this node's audio here: sources
    /// before their fader (pre-fader), sinks after (post-fader). Taken with
    /// `try_lock` so the audio path never waits.
    record_tap: Mutex<Option<Producer<f32>>>,
}

impl NodeParams {
    pub fn set(&self, gain_db: f32, mute: bool, mono: bool, reverse: bool) {
        self.gain.store(10f32.powf(gain_db / 20.0));
        self.mute.store(mute, Ordering::Relaxed);
        self.mono.store(mono, Ordering::Relaxed);
        self.reverse.store(reverse, Ordering::Relaxed);
    }

    pub fn set_delay_ms(&self, ms: f32) {
        let frames = (ms.max(0.0) * SAMPLE_RATE as f32 / 1000.0) as usize;
        self.delay_frames.store(frames.min(MAX_DELAY_FRAMES) as u32, Ordering::Relaxed);
    }

    pub fn stream(&self) -> Option<StreamInfo> {
        *self.stream.lock().ok()?
    }

    fn set_stream(&self, info: Option<StreamInfo>) {
        if let Ok(mut slot) = self.stream.lock() {
            *slot = info;
        }
    }

    /// Start (Some) or stop (None) copying this node's audio into a recording ring.
    pub fn set_record_tap(&self, tap: Option<Producer<f32>>) {
        if let Ok(mut slot) = self.record_tap.lock() {
            *slot = tap;
        }
    }

    /// Copy audio to the recording ring, if this node is being recorded.
    fn tap(&self, buf: &[f32]) {
        if let Ok(mut tap) = self.record_tap.try_lock()
            && let Some(tap) = tap.as_mut()
        {
            // A full ring means the writer fell behind; dropping beats blocking audio.
            let _ = tap.push_entire_slice(buf);
        }
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok()?.clone()
    }

    pub fn fail(&self, message: String) {
        log::error!("{message}");
        if let Ok(mut error) = self.error.lock() {
            *error = Some(message);
        }
    }

    pub fn take_peaks(&self) -> Vec<f32> {
        self.peaks.iter().map(|p| f32::from_bits(p.swap(0, Ordering::Relaxed))).collect()
    }

    /// Apply gain/mute/mono/reverse to interleaved stereo samples in place and record peaks.
    fn process(&self, buf: &mut [f32]) {
        let gain = if self.mute.load(Ordering::Relaxed) { 0.0 } else { self.gain.load() };
        let mono = self.mono.load(Ordering::Relaxed);
        let reverse = self.reverse.load(Ordering::Relaxed);
        let mut peak = [0f32; CHANNELS];
        for frame in buf.as_chunks_mut::<CHANNELS>().0 {
            let (mut l, mut r) = (frame[0] * gain, frame[1] * gain);
            if mono {
                l = (l + r) * 0.5;
                r = l;
            } else if reverse {
                std::mem::swap(&mut l, &mut r);
            }
            frame[0] = l;
            frame[1] = r;
            peak[0] = peak[0].max(l.abs());
            peak[1] = peak[1].max(r.abs());
        }
        for (slot, p) in self.peaks.iter().zip(peak) {
            slot.fetch_max(p.to_bits(), Ordering::Relaxed);
        }
    }
}

pub struct Params {
    pub sources: Vec<NodeParams>,
    pub sinks: Vec<NodeParams>,
    /// `routes[source * sinks.len() + sink]`
    routes: Vec<AtomicBool>,
    /// Audio a sink keeps buffered per source; also the prefill level. Larger means
    /// more latency but more tolerance for scheduling hiccups. Fixed per engine run.
    target_frames: usize,
}

impl Params {
    pub fn new(sources: usize, sinks: usize, target_frames: usize) -> Self {
        Self {
            sources: (0..sources).map(|_| NodeParams::default()).collect(),
            sinks: (0..sinks).map(|_| NodeParams::default()).collect(),
            routes: (0..sources * sinks).map(|_| AtomicBool::new(false)).collect(),
            target_frames: target_frames.clamp(240, RING_FRAMES / 4),
        }
    }

    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    /// Backlog at a ring's low point that is skipped in one go: half the target,
    /// but no less than the low point wanders by itself.
    fn snap_frames(&self) -> usize {
        (self.target_frames / 2).max(MIN_SNAP_FRAMES)
    }

    pub fn set_route(&self, source: usize, sink: usize, on: bool) {
        self.routes[source * self.sinks.len() + sink].store(on, Ordering::Relaxed);
    }

    fn route(&self, source: usize, sink: usize) -> bool {
        self.routes[source * self.sinks.len() + sink].load(Ordering::Relaxed)
    }
}

/// Opens platform devices for the engine. Each call runs on its own audio thread
/// and returns when the stream's [`is_running`](CaptureStream::is_running) goes
/// false or the device fails.
pub trait DeviceIo: Send + Sync + 'static {
    /// Record from `device`, handing every block to [`CaptureStream::push`].
    fn capture(&self, device: &str, stream: CaptureStream);
    /// Play into `device`, filling every block with [`RenderStream::fill`].
    fn render(&self, device: &str, stream: RenderStream);
}

type Ready = mpsc::Sender<std::result::Result<(), String>>;

/// Shared start/stop bookkeeping for one device stream.
struct StreamState {
    params: Arc<Params>,
    running: Arc<AtomicBool>,
    /// Taken once the device reports whether it started.
    ready: Option<Ready>,
}

impl StreamState {
    fn started(&mut self, node: &NodeParams, info: StreamInfo) {
        node.set_stream(Some(info));
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(Ok(()));
        }
    }

    fn failed(&mut self, node: &NodeParams, message: String) {
        node.fail(message.clone());
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(Err(message));
        }
    }
}

/// One source device's side of the engine, handed to [`DeviceIo::capture`].
pub struct CaptureStream {
    index: usize,
    outs: Vec<Producer<f32>>,
    state: StreamState,
}

impl CaptureStream {
    fn node(&self) -> &NodeParams {
        &self.state.params.sources[self.index]
    }

    pub fn is_running(&self) -> bool {
        self.state.running.load(Ordering::Relaxed)
    }

    /// The device opened: report it as running.
    pub fn started(&mut self, info: StreamInfo) {
        let params = self.state.params.clone();
        self.state.started(&params.sources[self.index], info);
    }

    /// The device failed to open or stopped; shown on the strip.
    pub fn failed(&mut self, message: String) {
        let params = self.state.params.clone();
        self.state.failed(&params.sources[self.index], message);
    }

    /// Hand captured interleaved stereo audio to the mix. Processes `buf` in place.
    pub fn push(&mut self, buf: &mut [f32]) {
        let node = &self.state.params.sources[self.index];
        node.tap(buf);
        node.process(buf);
        for out in &mut self.outs {
            // All-or-nothing so rings always hold whole frames; a full ring
            // means that sink stalled, so dropping is the right call.
            let _ = out.push_entire_slice(buf);
        }
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.node().set_stream(None);
    }
}

/// The lowest and highest fill a ring showed over the last one to two windows.
#[derive(Clone, Copy)]
struct FillWindow {
    /// (low, high) of the window being filled, and of the one before it.
    current: (usize, usize),
    previous: Option<(usize, usize)>,
    /// Frames until `current` becomes `previous`.
    left: usize,
}

impl FillWindow {
    fn new(fill: usize) -> Self {
        Self { current: (fill, fill), previous: None, left: WINDOW_FRAMES }
    }

    /// Note the fill at the start of a block of `frames`. True when a window completed.
    fn observe(&mut self, fill: usize, frames: usize) -> bool {
        self.current = (self.current.0.min(fill), self.current.1.max(fill));
        if self.left > frames {
            self.left -= frames;
            return false;
        }
        self.previous = Some(self.current);
        self.current = (fill, fill);
        self.left = WINDOW_FRAMES;
        true
    }

    fn low(&self) -> usize {
        self.previous.map_or(self.current.0, |p| p.0.min(self.current.0))
    }

    fn high(&self) -> usize {
        self.previous.map_or(self.current.1, |p| p.1.max(self.current.1))
    }

    /// A whole window has been seen, so `low` really is a low point.
    fn settled(&self) -> bool {
        self.previous.is_some()
    }

    /// `frames` were just skipped: every level seen would have been that much lower.
    fn lower(&mut self, frames: usize) {
        for (low, high) in std::iter::once(&mut self.current).chain(&mut self.previous) {
            *low = low.saturating_sub(frames);
            *high = high.saturating_sub(frames);
        }
    }
}

/// One source as seen by a sink: its ring plus the state that regulates it.
struct SourceReader {
    source: usize,
    ring: Consumer<f32>,
    primed: bool,
    /// The ring's fill at the start of recent blocks.
    window: FillWindow,
    /// Smoothed low point of the fill; the rate correction steers it to the floor.
    level: f32,
    /// How far the fill swings above its low point (the source's lumpiness).
    /// Remembered across re-buffering, which waits for this much extra.
    swing: usize,
    /// Extra held under the low point because this ring has run dry lately: the
    /// source sometimes delivers later than its usual pattern.
    cushion: f32,
    resampler: DriftResampler,
    /// Fade the next block in: it follows a gap or a skip.
    fade_in: bool,
    /// Fed by a device with its own clock. The file player isn't: it tops the ring
    /// up on demand, so it's played as-is with no drift correction or backlog skipping.
    clocked: bool,
}

impl SourceReader {
    fn new(source: usize, ring: Consumer<f32>, clocked: bool) -> Self {
        Self { source, ring, primed: false, window: FillWindow::new(0), level: 0.0, swing: 0, cushion: 0.0, resampler: DriftResampler::new(), fade_in: false, clocked }
    }

    /// Start (or restart) playing from a ring holding `buffered` frames.
    fn prime(&mut self, buffered: usize) {
        self.primed = true;
        self.window = FillWindow::new(buffered);
        self.level = buffered.saturating_sub(self.swing) as f32;
        self.resampler.reset();
        self.fade_in = true;
    }

    /// Discard `frames` of queued audio. What follows doesn't continue what was
    /// playing, so the resampler starts afresh and the next block fades in.
    fn skip(&mut self, frames: usize, stats: &NodeParams) -> bool {
        let Ok(chunk) = self.ring.read_chunk(frames * CHANNELS) else { return false };
        chunk.commit_all();
        stats.dropped_frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.resampler.reset();
        self.fade_in = true;
        true
    }
}

/// Ramp the first frames of `block` up from silence.
fn fade_in(block: &mut [f32]) {
    let frames = (block.len() / CHANNELS).min(SNAP_FADE_FRAMES);
    for (i, frame) in block.as_chunks_mut::<CHANNELS>().0.iter_mut().take(frames).enumerate() {
        let gain = i as f32 / frames as f32;
        frame.iter_mut().for_each(|x| *x *= gain);
    }
}

/// Ramp the last frames of `block` down to silence.
fn fade_out(block: &mut [f32]) {
    let frames = (block.len() / CHANNELS).min(SNAP_FADE_FRAMES);
    for (i, frame) in block.as_chunks_mut::<CHANNELS>().0.iter_mut().rev().take(frames).enumerate() {
        let gain = i as f32 / frames as f32;
        frame.iter_mut().for_each(|x| *x *= gain);
    }
}

/// One sink device's side of the engine, handed to [`DeviceIo::render`].
pub struct RenderStream {
    index: usize,
    ins: Vec<SourceReader>,
    delay_line: Vec<f32>,
    delay_pos: usize,
    scratch: Vec<f32>,
    /// The largest block the device asked for lately (decays slowly).
    block_max: f32,
    state: StreamState,
}

impl RenderStream {
    fn node(&self) -> &NodeParams {
        &self.state.params.sinks[self.index]
    }

    pub fn is_running(&self) -> bool {
        self.state.running.load(Ordering::Relaxed)
    }

    /// Whether this output should be opened exclusively (bypassing the OS mixer).
    pub fn exclusive(&self) -> bool {
        self.node().exclusive.load(Ordering::Relaxed)
    }

    /// The device opened: report it as running. `max_frames` is the largest block
    /// [`fill`](Self::fill) will be asked for, so the audio path never allocates.
    pub fn started(&mut self, info: StreamInfo, max_frames: usize) {
        self.scratch.resize(max_frames * CHANNELS, 0.0);
        for input in &mut self.ins {
            input.resampler.reserve(max_frames);
        }
        let params = self.state.params.clone();
        self.state.started(&params.sinks[self.index], info);
    }

    /// The device failed to open or stopped; shown on the bus.
    pub fn failed(&mut self, message: String) {
        let params = self.state.params.clone();
        self.state.failed(&params.sinks[self.index], message);
    }

    /// Mix the next block of interleaved stereo audio into `mix` (overwriting it).
    pub fn fill(&mut self, mix: &mut [f32]) {
        let params = &*self.state.params;
        let k = self.index;
        let stats = &params.sinks[k];
        let frames = mix.len() / CHANNELS;
        let n = frames * CHANNELS;
        if self.scratch.len() < n {
            self.scratch.resize(n, 0.0);
        }
        mix.fill(0.0);

        // A ring must always hold what the largest block needs, so the low point is
        // held at the user's buffer or, if the device asks for more at once, above that.
        self.block_max = (self.block_max * BLOCK_DECAY).max(frames as f32);
        let block_max = self.block_max.ceil() as usize;
        let floor = params.target_frames.max(block_max + DriftResampler::LOOKAHEAD + MARGIN_FRAMES);
        let (mut drift, mut swing, mut held_low) = (0f64, 0, floor);

        for input in &mut self.ins {
            let mut buffered = input.ring.slots() / CHANNELS;
            // This ring's own low point: the floor plus whatever its stalls have earned.
            let floor = floor + input.cushion as usize;
            if !input.clocked {
                // The file player: its feeder keeps the ring topped up, so play it 1:1.
                if !input.primed {
                    if buffered < params.target_frames.max(frames + DriftResampler::LOOKAHEAD) {
                        continue;
                    }
                    input.prime(buffered);
                }
            } else {
                // Far more queued than even a crest should reach (e.g. audio queued
                // before this sink started): skip it now rather than play it late.
                let crest = floor + input.swing;
                if buffered > crest + EMERGENCY_EXTRA_FRAMES && input.skip(buffered - crest, stats) {
                    buffered = crest;
                    input.primed = false;
                }
                if !input.primed {
                    // Wait for the low point plus the swing seen before: a source that
                    // delivers in bursts is otherwise started on a crest and runs dry
                    // before its next burst, over and over.
                    if buffered < crest {
                        continue;
                    }
                    input.prime(buffered);
                }
                // Audio held in the resampler counts as buffered too.
                let fill = buffered + input.resampler.buffered().max(0.0) as usize;
                let window_done = input.window.observe(fill, frames);
                let (low, high) = (input.window.low(), input.window.high());
                input.level += (low as f32 - input.level) * LEVEL_SMOOTHING;
                // Follow the swing up at once; let it and the cushion shrink a little per window.
                if window_done {
                    input.swing -= input.swing / 32;
                    input.cushion *= CUSHION_DECAY;
                }
                input.swing = input.swing.max(high - low);
                swing = swing.max(input.swing);
                held_low = held_low.max(floor);
            }

            // Drift correction: consume input slightly faster (source fast, low point
            // above the floor) or slower than we play it.
            let ratio = if input.clocked { 1.0 + ((input.level - floor as f32) as f64 * DRIFT_GAIN).clamp(-MAX_DRIFT, MAX_DRIFT) } else { 1.0 };
            if (ratio - 1.0).abs() > drift.abs() {
                drift = ratio - 1.0;
            }

            let need = input.resampler.needed(frames, ratio);
            let slot = input.resampler.input_slot(need);
            let got = input.ring.pop_partial_slice(slot).0.len();
            input.resampler.commit(need, got);
            if got < need * CHANNELS {
                // Underrun: drop this block and re-buffer before mixing this source again,
                // holding more from now on so the same stall doesn't empty the ring again.
                input.primed = false;
                input.cushion = (input.cushion + frames.max(CUSHION_STEP_FRAMES) as f32).min(MAX_CUSHION_FRAMES);
                stats.underruns.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let out = &mut self.scratch[..n];
            input.resampler.process(out, ratio);
            if std::mem::take(&mut input.fade_in) {
                fade_in(out);
            }
            // A backlog: even the low point is well above the floor, which the rate
            // correction would take most of a minute to fix (this sink was held up
            // while the source kept delivering). Skip the excess, fading around the cut.
            let low = input.window.low();
            if input.clocked && input.window.settled() && low > floor + params.snap_frames() {
                let excess = (low - floor).min(input.ring.slots() / CHANNELS);
                fade_out(out);
                if input.skip(excess, stats) {
                    input.window.lower(excess);
                    input.level = floor as f32;
                }
            }
            if params.route(input.source, k) {
                for (o, x) in mix.iter_mut().zip(out.iter()) {
                    *o += *x;
                }
            }
        }
        stats.drift_ppm.store((drift * 1e6) as f32);
        stats.floor_frames.store(held_low as u32, Ordering::Relaxed);
        stats.swing_frames.store(swing as u32, Ordering::Relaxed);
        stats.max_block.store(block_max as u32, Ordering::Relaxed);

        stats.process(mix);
        stats.tap(mix);
        let delay = stats.delay_frames.load(Ordering::Relaxed) as usize;
        apply_delay(&mut self.delay_line, &mut self.delay_pos, delay, mix);
    }
}

impl Drop for RenderStream {
    fn drop(&mut self) {
        self.node().set_stream(None);
    }
}

/// What feeds one engine source.
pub enum SourceSpec {
    Unbound,
    /// A platform capture device ID.
    Device(String),
    /// The recorder's file player.
    Player(Arc<Player>),
}

impl SourceSpec {
    fn is_bound(&self) -> bool {
        !matches!(self, SourceSpec::Unbound)
    }
}

/// Running audio threads. Dropping it stops and joins them.
pub struct Engine {
    running: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Engine {
    /// Start streams for every bound source and sink. `sinks[j]` are platform
    /// device IDs (or `None` if unbound); indices match those in `params`.
    /// Returns the engine plus one message per device that failed to start.
    pub fn start(io: Arc<dyn DeviceIo>, sources: &[SourceSpec], sinks: &[Option<String>], params: Arc<Params>) -> (Self, Vec<String>) {
        let running = Arc::new(AtomicBool::new(true));
        let mut producers: Vec<Vec<Producer<f32>>> = sources.iter().map(|_| Vec::new()).collect();
        let mut consumers: Vec<Vec<(usize, Consumer<f32>)>> = sinks.iter().map(|_| Vec::new()).collect();
        let bound_sinks: Vec<usize> = (0..sinks.len()).filter(|&k| sinks[k].is_some()).collect();
        for s in (0..sources.len()).filter(|&s| sources[s].is_bound()) {
            for &k in &bound_sinks {
                let (tx, rx) = RingBuffer::new(RING_FRAMES * CHANNELS);
                producers[s].push(tx);
                consumers[k].push((s, rx));
            }
        }

        let (ready_tx, ready_rx) = mpsc::channel();
        let state = |ready: &Ready| StreamState { params: params.clone(), running: running.clone(), ready: Some(ready.clone()) };
        let mut threads = Vec::new();
        let mut expected = 0;
        for ((s, source), outs) in sources.iter().enumerate().zip(producers) {
            match source {
                SourceSpec::Unbound => {}
                SourceSpec::Device(device) => {
                    let (io, device) = (io.clone(), device.clone());
                    let stream = CaptureStream { index: s, outs, state: state(&ready_tx) };
                    let params = params.clone();
                    threads.push(spawn(format!("capture-{s}"), move || guarded(&params.sources[s], || io.capture(&device, stream))));
                    expected += 1;
                }
                SourceSpec::Player(player) => {
                    let (player, params, running) = (player.clone(), params.clone(), running.clone());
                    threads.push(spawn(format!("player-{s}"), move || feed_player(&player, s, &params, outs, &running)));
                }
            }
        }
        for ((k, device), ins) in sinks.iter().enumerate().zip(consumers) {
            let Some(device) = device.clone() else { continue };
            let ins = ins.into_iter().map(|(source, ring)| SourceReader::new(source, ring, matches!(sources[source], SourceSpec::Device(_)))).collect();
            let stream = RenderStream {
                index: k,
                ins,
                delay_line: vec![0f32; (MAX_DELAY_FRAMES + 1) * CHANNELS],
                delay_pos: 0,
                scratch: Vec::new(),
                block_max: 0.0,
                state: state(&ready_tx),
            };
            let (io, params) = (io.clone(), params.clone());
            threads.push(spawn(format!("render-{k}"), move || guarded(&params.sinks[k], || io.render(&device, stream))));
            expected += 1;
        }
        drop(ready_tx);

        let mut errors = Vec::new();
        for _ in 0..expected {
            match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => errors.push(e),
                Err(_) => {
                    errors.push("an audio device did not start in time".into());
                    break;
                }
            }
        }
        (Self { running, threads }, errors)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Run a device's audio loop. If it panics, report that on the node like any other
/// device failure (so it shows in the UI and auto-restart can act) instead of
/// leaving the stream silently dead.
fn guarded(node: &NodeParams, audio_loop: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(audio_loop)).is_err() {
        node.fail("the audio thread crashed; restart the audio engine (Menu)".into());
    }
}

fn spawn(name: String, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new().name(name).spawn(f).expect("failed to spawn audio thread")
}

/// Write `buf` into the delay line and replace it with audio from `delay` frames
/// ago. The line is always written, so turning a delay on plays recent audio
/// rather than stale samples.
fn apply_delay(line: &mut [f32], pos: &mut usize, delay: usize, buf: &mut [f32]) {
    let len = line.len();
    let back = delay * CHANNELS;
    for x in buf.iter_mut() {
        line[*pos] = *x;
        if back > 0 {
            *x = line[(*pos + len - back) % len];
        }
        *pos = (*pos + 1) % len;
    }
}

/// Feed the file player into the sink rings. The player has no clock of its own,
/// so it simply keeps every ring a little above the sinks' target fill (enough for
/// the largest block any sink asks for plus the resampler's lookahead); each sink
/// then plays it 1:1 at its own device rate.
fn feed_player(player: &Player, s: usize, params: &Params, mut outs: Vec<Producer<f32>>, running: &AtomicBool) {
    const BLOCK_FRAMES: usize = 480;
    let mut buf = vec![0f32; BLOCK_FRAMES * CHANNELS];
    while running.load(Ordering::Relaxed) {
        let fill = outs.iter().map(|o| RING_FRAMES - o.slots() / CHANNELS).min();
        let largest_block = params.sinks.iter().map(|sink| sink.max_block.load(Ordering::Relaxed) as usize).max().unwrap_or(0);
        let wants_audio = fill.is_some_and(|f| f < params.target_frames.max(largest_block) + DriftResampler::LOOKAHEAD);
        let frames = if wants_audio { player.read(&mut buf) } else { 0 };
        if frames == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let block = &mut buf[..frames * CHANNELS];
        params.sources[s].process(block);
        for out in &mut outs {
            let _ = out.push_entire_slice(block);
        }
    }
}

/// Which devices are bound to which graph nodes. The engine only restarts when this changes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Topology {
    pub sources: Vec<(String, Option<crate::DeviceId>)>,
    pub sinks: Vec<(String, Option<crate::DeviceId>)>,
    /// Per sink: open exclusively. Changing it reopens the device, so it's part of the topology.
    pub sink_exclusive: Vec<bool>,
}

impl Topology {
    pub fn of(graph: &RoutingGraph) -> Self {
        let bind = |nodes: &[GraphNode]| nodes.iter().map(|n| (n.key.clone(), n.device.clone())).collect();
        let sink_exclusive = graph.sinks.iter().map(|n| n.exclusive).collect();
        Self { sources: bind(&graph.sources), sinks: bind(&graph.sinks), sink_exclusive }
    }
}

struct Running {
    topology: Topology,
    params: Arc<Params>,
    /// Per node (sources, then sinks): whether the engine opened a device for it.
    /// Unlike `topology`, this includes nodes the backend binds itself (e.g. created virtual devices).
    bound: Vec<bool>,

    // Dropped last: stops the audio threads.
    _engine: Engine,
}

impl Running {
    /// `(key, bound, params)` for every source, then every sink.
    fn nodes(&self) -> impl Iterator<Item = (&String, bool, &NodeParams)> {
        let sources = self.topology.sources.iter().zip(&self.params.sources);
        let sinks = self.topology.sinks.iter().zip(&self.params.sinks);
        sources.chain(sinks).zip(&self.bound).map(|(((key, _), p), &bound)| (key, bound, p))
    }

    /// Point each recordable node's audio thread at a new ring; returns the readers.
    /// A node is recordable when it has a device and its stream is running.
    fn attach_record_taps(&self, keys: &[String]) -> Vec<(String, Consumer<f32>)> {
        let mut readers = Vec::new();
        for (key, bound, params) in self.nodes() {
            if keys.contains(key) && bound && params.stream().is_some() {
                let (tx, rx) = RingBuffer::new(RECORD_RING_SAMPLES);
                params.set_record_tap(Some(tx));
                readers.push((key.clone(), rx));
            }
        }
        readers
    }
}

/// Recording rings by node key, shared with the app's writer thread. The host
/// swaps in fresh rings when the engine restarts mid-recording.
type RecordRings = Arc<Mutex<Vec<(String, Consumer<f32>)>>>;

/// The engine plus everything around it that doesn't depend on the platform:
/// the file player, recording, meters and stats. Backends resolve devices and
/// call [`EngineHost::start`]; the rest of [`crate::AudioBackend`] delegates here.
pub struct EngineHost {
    io: Arc<dyn DeviceIo>,
    running: Option<Running>,
    /// Buffering per source, from the user's latency setting.
    target_frames: usize,
    player: Arc<Player>,
    /// Nodes being recorded and the rings the writer reads.
    recording: Option<(Vec<String>, RecordRings)>,
}

impl EngineHost {
    pub fn new(io: Arc<dyn DeviceIo>) -> Self {
        Self { io, running: None, target_frames: DEFAULT_TARGET_FRAMES, player: Arc::default(), recording: None }
    }

    /// Whether the engine must (re)start to realise `topology`.
    pub fn needs_start(&self, topology: &Topology) -> bool {
        self.running.as_ref().is_none_or(|r| &r.topology != topology)
    }

    /// Stop the engine; the next `apply` starts a fresh one.
    pub fn stop(&mut self) {
        self.running = None;
    }

    /// Start the engine for `topology`. `sources` and `sinks` are the platform
    /// device IDs to open per node (`None` = unbound; the player node is fed
    /// automatically). `disabled` lists sources that must not run, with the reason.
    pub fn start(&mut self, topology: Topology, sources: Vec<Option<String>>, sinks: Vec<Option<String>>, disabled: Vec<(usize, String)>) {
        // Stop the old engine first so its devices are released.
        self.running = None;
        let params = Arc::new(Params::new(topology.sources.len(), topology.sinks.len(), self.target_frames));
        for (sink, &exclusive) in params.sinks.iter().zip(&topology.sink_exclusive) {
            sink.exclusive.store(exclusive, Ordering::Relaxed);
        }
        let mut sources = sources;
        for (s, reason) in disabled {
            sources[s] = None;
            params.sources[s].fail(reason);
        }
        let specs: Vec<SourceSpec> = topology
            .sources
            .iter()
            .zip(sources)
            .map(|((key, _), id)| match id {
                _ if key == PLAYER_KEY => SourceSpec::Player(self.player.clone()),
                Some(id) => SourceSpec::Device(id),
                None => SourceSpec::Unbound,
            })
            .collect();
        let bound = specs.iter().map(|s| matches!(s, SourceSpec::Device(_))).chain(sinks.iter().map(Option::is_some)).collect();
        // Failures are recorded per node in `params`; see `node_errors`.
        let (engine, _) = Engine::start(self.io.clone(), &specs, &sinks, params.clone());
        let running = Running { topology, params, bound, _engine: engine };
        if let Some((keys, rings)) = &self.recording {
            // Keep an in-progress recording going on the new engine.
            let fresh = running.attach_record_taps(keys);
            if let Ok(mut rings) = rings.lock() {
                *rings = fresh;
            }
        }
        self.running = Some(running);
    }

    /// Set gains, mutes, delays and routes from `graph` on the running engine.
    /// Errors list the nodes whose devices aren't working.
    pub fn set_controls(&self, graph: &RoutingGraph) -> Result<()> {
        let Some(running) = &self.running else { return Ok(()) };
        for (node, p) in graph.sources.iter().zip(&running.params.sources) {
            p.set(node.gain_db, node.mute, node.mono, node.reverse);
        }
        for (node, p) in graph.sinks.iter().zip(&running.params.sinks) {
            p.set(node.gain_db, node.mute, node.mono, node.reverse);
            p.set_delay_ms(node.delay_ms);
        }

        let index = |nodes: &[GraphNode], key: &str| nodes.iter().position(|n| n.key == key);
        let wanted: HashSet<(usize, usize)> = graph
            .routes
            .iter()
            .filter_map(|(s, k)| Some((index(&graph.sources, s)?, index(&graph.sinks, k)?)))
            .collect();
        for s in 0..graph.sources.len() {
            for k in 0..graph.sinks.len() {
                running.params.set_route(s, k, wanted.contains(&(s, k)));
            }
        }

        let problems: Vec<String> = self.node_errors().into_iter().map(|(key, error)| format!("{key}: {error}")).collect();
        if problems.is_empty() { Ok(()) } else { Err(BackendError::Platform(problems.join("; "))) }
    }

    pub fn levels(&self) -> Levels {
        let Some(running) = &self.running else { return Levels::new() };
        running.nodes().map(|(key, _, p)| (key.clone(), p.take_peaks())).collect()
    }

    pub fn node_errors(&self) -> HashMap<String, String> {
        let Some(running) = &self.running else { return Default::default() };
        running.nodes().filter_map(|(key, _, p)| Some((key.clone(), p.error()?))).collect()
    }

    pub fn stream_info(&self) -> HashMap<String, StreamInfo> {
        let Some(running) = &self.running else { return Default::default() };
        running.nodes().filter_map(|(key, _, p)| Some((key.clone(), p.stream()?))).collect()
    }

    pub fn set_buffer_ms(&mut self, ms: u32) {
        let frames = SAMPLE_RATE * ms as usize / 1000;
        if frames != self.target_frames {
            self.target_frames = frames;
            self.running = None;
        }
    }

    pub fn player(&self) -> Arc<Player> {
        self.player.clone()
    }

    pub fn start_recording(&mut self, keys: &[String]) -> Result<Box<dyn RecordingStream>> {
        self.stop_recording();
        let readers = self.running.as_ref().map(|r| r.attach_record_taps(keys)).unwrap_or_default();
        if readers.is_empty() {
            return Err(BackendError::Platform("none of the armed inputs or buses has a working device".into()));
        }
        let rings: RecordRings = Arc::new(Mutex::new(readers));
        self.recording = Some((keys.to_vec(), rings.clone()));
        Ok(Box::new(NodeRecording { rings }))
    }

    pub fn stop_recording(&mut self) {
        self.recording = None;
        if let Some(running) = &self.running {
            for (_, _, node) in running.nodes() {
                node.set_record_tap(None);
            }
        }
    }

    pub fn stats(&self) -> Vec<String> {
        let Some(running) = &self.running else { return Vec::new() };
        let sinks = running.nodes().skip(running.params.sources.len());
        sinks
            .filter(|&(_, bound, _)| bound)
            .map(|(key, _, p)| {
                let underruns = p.underruns.load(Ordering::Relaxed);
                let dropped = p.dropped_frames.load(Ordering::Relaxed);
                let ppm = p.drift_ppm.load();
                let ms = |frames: &AtomicU32| frames.load(Ordering::Relaxed) as f32 * 1000.0 / SAMPLE_RATE as f32;
                let (floor, swing, block) = (ms(&p.floor_frames), ms(&p.swing_frames), ms(&p.max_block));
                format!(
                    "{key}: {underruns} underruns, clock drift {ppm:+.0} ppm, {dropped} backlog frames dropped, buffer {floor:.0}+{swing:.0} ms, block {block:.0} ms"
                )
            })
            .collect()
    }
}

/// Reads the recorded nodes. Devices run on their own clocks, so each read
/// takes as much as every node has ready; a node that stops delivering (device
/// lost) is dropped after a second so the others keep recording.
struct NodeRecording {
    rings: RecordRings,
}

impl NodeRecording {
    /// Samples ready in every ring (whole frames), after dropping stalled rings.
    fn ready(rings: &mut Vec<(String, Consumer<f32>)>) -> usize {
        const STALL_SAMPLES: usize = SAMPLE_RATE * CHANNELS;
        let most = rings.iter().map(|(_, r)| r.slots()).max().unwrap_or(0);
        if most > STALL_SAMPLES {
            rings.retain(|(_, r)| r.slots() > 0);
        }
        rings.iter().map(|(_, r)| r.slots()).min().unwrap_or(0) / CHANNELS * CHANNELS
    }
}

impl RecordingStream for NodeRecording {
    fn read(&mut self, out: &mut Vec<f32>) -> usize {
        let Ok(mut rings) = self.rings.lock() else { return 0 };
        let n = Self::ready(&mut rings);
        if n == 0 {
            return 0;
        }
        let start = out.len();
        out.resize(start + n, 0.0);
        for (_, ring) in rings.iter_mut() {
            if let Ok(chunk) = ring.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                for (o, x) in out[start..].iter_mut().zip(a.iter().chain(b)) {
                    *o += *x;
                }
                chunk.commit_all();
            }
        }
        n / CHANNELS
    }

    fn read_tracks(&mut self) -> Vec<(String, Vec<f32>)> {
        let Ok(mut rings) = self.rings.lock() else { return Vec::new() };
        let n = Self::ready(&mut rings);
        rings
            .iter_mut()
            .map(|(key, ring)| {
                let mut samples = Vec::with_capacity(n);
                if n > 0
                    && let Ok(chunk) = ring.read_chunk(n)
                {
                    let (a, b) = chunk.as_slices();
                    samples.extend_from_slice(a);
                    samples.extend_from_slice(b);
                    chunk.commit_all();
                }
                (key.clone(), samples)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_applies_gain_mute_mono_and_peaks() {
        let p = NodeParams::default();
        p.set(-6.0206, false, false, false); // x0.5
        let mut buf = [1.0, -0.5, 0.2, 0.4];
        p.process(&mut buf);
        assert!((buf[0] - 0.5).abs() < 1e-4 && (buf[1] + 0.25).abs() < 1e-4);
        let peaks = p.take_peaks();
        assert!((peaks[0] - 0.5).abs() < 1e-4 && (peaks[1] - 0.25).abs() < 1e-4);
        assert_eq!(p.take_peaks(), vec![0.0, 0.0], "peaks reset after reading");

        p.set(0.0, false, true, false);
        let mut buf = [1.0, 0.0];
        p.process(&mut buf);
        assert_eq!(buf, [0.5, 0.5]);

        p.set(0.0, true, false, false);
        let mut buf = [1.0, 1.0];
        p.process(&mut buf);
        assert_eq!(buf, [0.0, 0.0]);
    }

    #[test]
    fn blocks_larger_than_the_target_still_play() {
        // A device may ask for its whole buffer at once, beyond the target fill;
        // priming must leave room for that plus the resampler's lookahead.
        let params = Arc::new(Params::new(1, 1, 480));
        params.set_route(0, 0, true);
        params.sinks[0].set(0.0, false, false, false);
        let (mut tx, rx) = RingBuffer::new(RING_FRAMES * CHANNELS);
        let state = StreamState { params: params.clone(), running: Arc::new(AtomicBool::new(true)), ready: None };
        let delay_line = vec![0.0; (MAX_DELAY_FRAMES + 1) * CHANNELS];
        let mut stream = RenderStream { index: 0, ins: vec![SourceReader::new(0, rx, true)], delay_line, delay_pos: 0, scratch: Vec::new(), block_max: 0.0, state };
        let block = vec![0.5f32; 961 * CHANNELS];
        let mut mix = vec![0f32; 960 * CHANNELS];
        let mut played = 0;
        for _ in 0..20 {
            tx.push_entire_slice(&block).unwrap();
            stream.fill(&mut mix);
            played += usize::from(mix[mix.len() - 1] > 0.49);
        }
        // Before, every prime left too little for such a block: it underran forever and played nothing.
        assert!(played >= 10, "played {played} of 20 blocks");
    }

    /// A sink with one clocked source, fed and drained by hand.
    fn manual_sink(target: usize) -> (Arc<Params>, Producer<f32>, RenderStream) {
        let params = Arc::new(Params::new(1, 1, target));
        params.set_route(0, 0, true);
        params.sinks[0].set(0.0, false, false, false);
        let (tx, rx) = RingBuffer::new(RING_FRAMES * CHANNELS);
        let state = StreamState { params: params.clone(), running: Arc::new(AtomicBool::new(true)), ready: None };
        let delay_line = vec![0.0; (MAX_DELAY_FRAMES + 1) * CHANNELS];
        let stream = RenderStream { index: 0, ins: vec![SourceReader::new(0, rx, true)], delay_line, delay_pos: 0, scratch: Vec::new(), block_max: 0.0, state };
        (params, tx, stream)
    }

    #[test]
    fn a_crashed_audio_thread_is_reported_on_its_node() {
        let node = NodeParams::default();
        guarded(&node, || {});
        assert_eq!(node.error(), None);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // keep the expected panic out of the test output
        guarded(&node, || panic!("boom"));
        std::panic::set_hook(hook);
        assert!(node.error().is_some_and(|e| e.contains("crashed")));
    }

    #[test]
    fn a_backlog_after_a_stall_is_cleared_quickly() {
        // The sink's thread stalls for 100 ms while the source keeps delivering:
        // 4800 extra frames queue up. Left to the rate correction alone (0.2 %)
        // they'd take about a minute to drain, and that output would play late
        // (an echo against any other output) the whole time.
        let (params, mut tx, mut stream) = manual_sink(960);
        let block = vec![0.25f32; 480 * CHANNELS];
        let mut mix = vec![0f32; 480 * CHANNELS];
        let mut run = |tx: &mut Producer<f32>, stream: &mut RenderStream, blocks: usize| {
            for _ in 0..blocks {
                tx.push_entire_slice(&block).unwrap();
                stream.fill(&mut mix);
            }
            stream.ins[0].ring.slots() / CHANNELS
        };
        let settled = run(&mut tx, &mut stream, 300); // 3 s: well past the warm-up
        assert!(settled <= 960, "steady state holds about the target: {settled}");

        for _ in 0..10 {
            tx.push_entire_slice(&block).unwrap(); // the stall
        }
        let late = run(&mut tx, &mut stream, 200); // 2 s later
        assert!(late <= 960 + 480, "backlog still {late} frames 2 s after a stall");
        assert_eq!(params.sinks[0].underruns.load(Ordering::Relaxed), 0);
    }

    /// What a simulated sink did once it had settled.
    struct Simulated {
        underruns: u64,
        dropped: u64,
        /// Lowest and highest ring fill at the start of a block, in frames.
        fill: (usize, usize),
    }

    /// Run a sink through simulated time. The source delivers `packet` frames at a
    /// time, running `ppm` fast; the sink asks for `block` frames at a time. The
    /// first seconds, where the engine learns the pattern, aren't counted.
    fn simulate(target: usize, packet: usize, block: usize, ppm: f64) -> Simulated {
        simulate_with_stalls(target, packet, block, ppm, None, 5, 20)
    }

    /// As [`simulate`], with the source also stalling: `stall = (every, frames)`
    /// holds every `every`-th delivery back by `frames`, the ones behind it too.
    fn simulate_with_stalls(target: usize, packet: usize, block: usize, ppm: f64, stall: Option<(usize, usize)>, settle: usize, seconds: usize) -> Simulated {
        let (params, mut tx, mut stream) = manual_sink(target);
        let sink = &params.sinks[0];
        let data = vec![0.25f32; packet * CHANNELS];
        let mut mix = vec![0f32; block * CHANNELS];
        let packet_time = packet as f64 / (1.0 + ppm * 1e-6);
        let (mut next_packet, mut next_block) = (0f64, 0usize);
        let mut settled = (0, 0);
        let mut fill = (usize::MAX, 0);
        let (mut delivered, mut held_until) = (0usize, 0f64);
        while next_block < seconds * SAMPLE_RATE {
            if next_packet.max(held_until) <= next_block as f64 {
                let _ = tx.push_entire_slice(&data);
                next_packet += packet_time;
                delivered += 1;
                if let Some((every, frames)) = stall
                    && delivered % every == 0
                {
                    held_until = next_packet + frames as f64;
                }
                continue;
            }
            if next_block < settle * SAMPLE_RATE {
                settled = (sink.underruns.load(Ordering::Relaxed), sink.dropped_frames.load(Ordering::Relaxed));
            } else {
                let queued = stream.ins[0].ring.slots() / CHANNELS;
                fill = (fill.0.min(queued), fill.1.max(queued));
            }
            stream.fill(&mut mix);
            next_block += block;
        }
        Simulated {
            underruns: sink.underruns.load(Ordering::Relaxed) - settled.0,
            dropped: sink.dropped_frames.load(Ordering::Relaxed) - settled.1,
            fill,
        }
    }

    #[test]
    fn lumpy_delivery_plays_without_gaps() {
        // (frames per delivery, frames per block). Sources hand over several packets
        // at once when their thread wakes late or the device has a long period; these
        // patterns used to make the engine drop the "excess" of each burst, run dry
        // before the next one and re-buffer, dozens of times a second.
        for (packet, block) in [(480, 480), (480, 448), (960, 480), (960, 448), (1440, 480), (1344, 448), (1920, 448), (1440, 128)] {
            let run = simulate(960, packet, block, 0.0);
            assert_eq!((run.underruns, run.dropped), (0, 0), "{packet} frames at a time into blocks of {block}");
            assert!(run.fill.0 >= block, "{packet}/{block}: ring ran down to {} frames", run.fill.0);
            // At most the skip threshold plus the cushion one start-up underrun earns.
            assert!(run.fill.0 <= 960 + 480 + 480, "{packet}/{block}: low point {} is far above the target", run.fill.0);
        }
    }

    #[test]
    fn blocks_larger_than_the_buffer_play_without_gaps() {
        // A shared-mode device asks for its whole buffer when our thread wakes late.
        for block in [960, 1024, 1056, 2048] {
            let run = simulate(960, 480, block, 0.0);
            assert_eq!((run.underruns, run.dropped), (0, 0), "blocks of {block}");
        }
        // And a small buffer setting can't starve a normal block either.
        let run = simulate(240, 480, 480, 0.0);
        assert_eq!((run.underruns, run.dropped), (0, 0), "5 ms buffer");
    }

    #[test]
    fn a_source_that_keeps_stalling_earns_a_cushion() {
        // Even delivery, but every 3 s the source's thread is held up for 25 ms and
        // then hands over everything at once. The default buffer has 10 ms to spare,
        // so the first stalls run the ring dry; after that the cushion absorbs them.
        let every = 300;
        let run = simulate_with_stalls(960, 480, 480, 0.0, Some((every, 1200)), 15, 60);
        assert_eq!(run.underruns, 0, "stalls still run the ring dry after 15 s");
        assert!(run.fill.1 < 960 + 4800, "crest {} frames: the cushion ran away", run.fill.1);
    }

    #[test]
    fn drifting_clocks_are_followed_without_gaps_or_skips() {
        for ppm in [-300.0, 300.0] {
            for (packet, block) in [(480, 480), (1440, 448)] {
                let run = simulate(960, packet, block, ppm);
                assert_eq!((run.underruns, run.dropped), (0, 0), "{ppm} ppm, {packet}/{block}");
            }
        }
    }

    #[test]
    fn reverse_swaps_channels_unless_mono() {
        let p = NodeParams::default();
        p.set(0.0, false, false, true);
        let mut buf = [1.0, 0.25];
        p.process(&mut buf);
        assert_eq!(buf, [0.25, 1.0]);
        p.set(0.0, false, true, true);
        let mut buf = [1.0, 0.0];
        p.process(&mut buf);
        assert_eq!(buf, [0.5, 0.5], "mono wins");
    }

    #[test]
    fn delay_shifts_audio_by_whole_frames() {
        let mut line = vec![0.0; 8 * CHANNELS];
        let mut pos = 0;
        let mut buf: Vec<f32> = (1..=6).flat_map(|i| [i as f32, -(i as f32)]).collect();
        apply_delay(&mut line, &mut pos, 2, &mut buf);
        assert_eq!(buf, [0.0, 0.0, 0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0]);
        let mut next = vec![7.0, -7.0];
        apply_delay(&mut line, &mut pos, 2, &mut next);
        assert_eq!(next, [5.0, -5.0], "continues across calls");
    }

    #[test]
    fn routes_are_indexed_per_source_and_sink() {
        let p = Params::new(2, 3, DEFAULT_TARGET_FRAMES);
        p.set_route(1, 2, true);
        assert!(p.route(1, 2));
        assert!(!p.route(0, 2) && !p.route(1, 1));
    }

    /// Loops audio straight from capture to render with no real devices: a
    /// capture thread pushes a constant signal, a render thread records what it mixes.
    struct FakeIo {
        rendered: Arc<Mutex<Vec<f32>>>,
    }

    impl DeviceIo for FakeIo {
        fn capture(&self, _device: &str, mut stream: CaptureStream) {
            let info = StreamInfo { sample_rate: 48_000, channels: 2, bits: 32, buffer_frames: 240, exclusive: false };
            stream.started(info);
            let mut block = vec![0.5f32; 240 * CHANNELS];
            while stream.is_running() {
                block.fill(0.5);
                stream.push(&mut block);
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn render(&self, _device: &str, mut stream: RenderStream) {
            let info = StreamInfo { sample_rate: 48_000, channels: 2, bits: 32, buffer_frames: 240, exclusive: false };
            stream.started(info, 240);
            let mut mix = vec![0f32; 240 * CHANNELS];
            while stream.is_running() {
                stream.fill(&mut mix);
                self.rendered.lock().unwrap().extend_from_slice(&mix);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn host_mixes_routed_sources_with_gain() {
        let rendered = Arc::new(Mutex::new(Vec::new()));
        let mut host = EngineHost::new(Arc::new(FakeIo { rendered: rendered.clone() }));
        let node = |key: &str, gain_db| GraphNode {
            key: key.into(),
            device: Some(crate::DeviceId(key.into())),
            create_virtual: None,
            gain_db,
            mute: false,
            mono: false,
            reverse: false,
            delay_ms: 0.0,
            exclusive: false,
        };
        let graph = RoutingGraph {
            sources: vec![node("mic", -6.0206)],
            sinks: vec![node("A1", 0.0)],
            routes: vec![("mic".into(), "A1".into())],
        };
        let topology = Topology::of(&graph);
        assert!(host.needs_start(&topology));
        host.start(topology.clone(), vec![Some("mic".into())], vec![Some("A1".into())], Vec::new());
        host.set_controls(&graph).unwrap();
        assert!(!host.needs_start(&topology));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(host.stream_info().len(), 2);
        let peaks = &host.levels()["A1"];
        assert!((peaks[0] - 0.25).abs() < 1e-3, "0.5 at -6 dB reaches the bus: {peaks:?}");
        host.stop();
        assert!(rendered.lock().unwrap().iter().any(|&x| (x - 0.25).abs() < 1e-3));
    }
}
