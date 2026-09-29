//! Device access for the shared mixing engine ([`openmeeter_backend::engine`]):
//! one PulseAudio stream per device, served by pipewire-pulse. The server
//! converts to the engine format (48 kHz stereo f32) and paces each thread with
//! its requests for (or deliveries of) audio.
//!
//! Each audio thread runs its own main loop, polled with a timeout so it notices
//! when the engine stops. Streams are pinned to their device: if it disappears
//! they stop and report it, rather than jumping to another device (a monitor
//! mix meant for headphones must not end up on the speakers).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libpulse_binding::context::{self, Context};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::error::PAErr;
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::{self, PeekResult, SeekMode, Stream};
use libpulse_binding::time::MicroSeconds;
use openmeeter_backend::StreamInfo;
use openmeeter_backend::engine::{CHANNELS, CaptureStream, DeviceIo, RenderStream, SAMPLE_RATE};

/// Frames per read/write (about 5 ms). Small requests also keep PipeWire's
/// graph quantum small, so audio arrives in small, even bursts.
const BLOCK_FRAMES: usize = 256;
const BYTES_PER_FRAME: usize = CHANNELS * 4;
const BLOCK_BYTES: usize = BLOCK_FRAMES * BYTES_PER_FRAME;
/// Audio queued in the server per playback stream, in blocks.
const PLAYBACK_BLOCKS: usize = 2;
/// How long each main loop pass waits for the server, and so how quickly a
/// thread notices the engine stopping.
const POLL_TIMEOUT: Duration = Duration::from_millis(100);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// A stream that moves no audio for this long has lost its device.
const STALL_TIMEOUT: Duration = Duration::from_secs(1);

pub struct Pulse {
    /// Native format per device name, refreshed on each engine start: what System
    /// Settings shows, and which devices exist.
    pub formats: Arc<Mutex<HashMap<String, StreamInfo>>>,
}

impl Pulse {
    /// Open a stream on `device` and describe it for System Settings.
    fn open(&self, direction: stream::Direction, device: &str, attr: &BufferAttr) -> Result<(Connection, StreamInfo), String> {
        // Connecting to a missing device may silently pick another, so check first.
        let native = self.formats.lock().ok().and_then(|f| f.get(device).copied());
        let Some(native) = native else {
            return Err(format!("{device}: the device wasn't found (unplugged or renamed?)"));
        };
        let connection = Connection::open(direction, device, attr).map_err(|e| format!("{device}: {e}"))?;
        Ok((connection, StreamInfo { buffer_frames: BLOCK_FRAMES as u32, ..native }))
    }
}

/// Plain-English text for the failures people actually hit.
fn explain(e: PAErr) -> String {
    let raw = e.to_string().unwrap_or_else(|| format!("error {}", e.0));
    match raw.as_str() {
        "No such entity" => "the device wasn't found (unplugged or renamed?)".to_string(),
        "Connection refused" | "Connection terminated" => "can't reach the sound server (is PipeWire running?)".to_string(),
        _ => raw,
    }
}

/// A server connection with one stream on it, driven by this thread.
struct Connection {
    mainloop: Mainloop,
    context: Context,
    stream: Stream,
}

impl Connection {
    fn open(direction: stream::Direction, device: &str, attr: &BufferAttr) -> Result<Self, String> {
        let mut mainloop = Mainloop::new().ok_or("could not create a PulseAudio main loop")?;
        let mut context = Context::new(&mainloop, "OpenMeeter").ok_or("could not create a PulseAudio context")?;
        context.connect(None, context::FlagSet::NOAUTOSPAWN, None).map_err(explain)?;
        wait(&mut mainloop, || match context.get_state() {
            context::State::Ready => Some(Ok(())),
            context::State::Failed | context::State::Terminated => Some(Err(explain(context.errno()))),
            _ => None,
        })?;

        let spec = Spec { format: Format::F32le, rate: SAMPLE_RATE as u32, channels: CHANNELS as u8 };
        let recording = direction == stream::Direction::Record;
        let name = if recording { "OpenMeeter capture" } else { "OpenMeeter playback" };
        let mut stream = Stream::new(&mut context, name, &spec, None).ok_or("could not create a stream")?;
        let flags = stream::FlagSet::DONT_MOVE | stream::FlagSet::ADJUST_LATENCY;
        let connected =
            if recording { stream.connect_record(Some(device), Some(attr), flags) } else { stream.connect_playback(Some(device), Some(attr), flags, None, None) };
        connected.map_err(explain)?;
        wait(&mut mainloop, || match stream.get_state() {
            stream::State::Ready => Some(Ok(())),
            stream::State::Failed | stream::State::Terminated => Some(Err(explain(context.errno()))),
            _ => None,
        })?;
        Ok(Self { mainloop, context, stream })
    }

    /// Wait up to [`POLL_TIMEOUT`] for the server and handle what it sent.
    fn iterate(&mut self) -> Result<(), String> {
        iterate(&mut self.mainloop, POLL_TIMEOUT)?;
        match self.stream.get_state() {
            stream::State::Failed | stream::State::Terminated => Err(explain(self.context.errno())),
            _ => Ok(()),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
        self.context.disconnect();
    }
}

fn iterate(mainloop: &mut Mainloop, timeout: Duration) -> Result<(), String> {
    mainloop.prepare(Some(MicroSeconds(timeout.as_micros() as u64))).map_err(explain)?;
    mainloop.poll().map_err(explain)?;
    mainloop.dispatch().map_err(explain)?;
    Ok(())
}

/// Run the main loop until `done` returns a result, or time out.
fn wait(mainloop: &mut Mainloop, mut done: impl FnMut() -> Option<Result<(), String>>) -> Result<(), String> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if let Some(result) = done() {
            return result;
        }
        if Instant::now() >= deadline {
            return Err("the sound server did not respond".into());
        }
        iterate(mainloop, POLL_TIMEOUT)?;
    }
}

impl DeviceIo for Pulse {
    fn capture(&self, device: &str, mut stream: CaptureStream) {
        let attr = BufferAttr { maxlength: u32::MAX, tlength: u32::MAX, prebuf: u32::MAX, minreq: u32::MAX, fragsize: BLOCK_BYTES as u32 };
        crate::rt::raise_current_thread();
        let (mut pa, info) = match self.open(stream::Direction::Record, device, &attr) {
            Ok(v) => v,
            Err(e) => return stream.failed(e),
        };
        stream.started(info);

        let mut samples: Vec<f32> = Vec::with_capacity(4 * BLOCK_FRAMES * CHANNELS);
        let mut last_audio = Instant::now();
        while stream.is_running() {
            if let Err(e) = pa.iterate() {
                stream.failed(format!("stopped: {device}: {e}"));
                break;
            }
            loop {
                samples.clear();
                match pa.stream.peek() {
                    Ok(PeekResult::Empty) => break,
                    // A gap in the recording: keep time with silence.
                    Ok(PeekResult::Hole(bytes)) => samples.resize(bytes / 4, 0.0),
                    Ok(PeekResult::Data(bytes)) => samples.extend(bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b))),
                    Err(e) => {
                        stream.failed(format!("stopped: {device}: {}", explain(e)));
                        return;
                    }
                }
                let _ = pa.stream.discard();
                let whole_frames = samples.len() / CHANNELS * CHANNELS;
                stream.push(&mut samples[..whole_frames]);
                last_audio = Instant::now();
            }
            if last_audio.elapsed() > STALL_TIMEOUT {
                stream.failed(format!("stopped: {device} stopped delivering audio (unplugged?)"));
                break;
            }
        }
    }

    fn render(&self, device: &str, mut stream: RenderStream) {
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: (BLOCK_BYTES * PLAYBACK_BLOCKS) as u32,
            prebuf: u32::MAX,
            minreq: BLOCK_BYTES as u32,
            fragsize: u32::MAX,
        };
        crate::rt::raise_current_thread();
        let (mut pa, info) = match self.open(stream::Direction::Playback, device, &attr) {
            Ok(v) => v,
            Err(e) => return stream.failed(e),
        };
        stream.started(info, BLOCK_FRAMES);

        let mut mix = vec![0f32; BLOCK_FRAMES * CHANNELS];
        let mut raw = vec![0u8; BLOCK_BYTES];
        let mut last_write = Instant::now();
        while stream.is_running() {
            if let Err(e) = pa.iterate() {
                stream.failed(format!("stopped: {device}: {e}"));
                break;
            }
            // The server asks for audio as it plays: this is what paces the sink.
            while pa.stream.writable_size().is_some_and(|n| n >= BLOCK_BYTES) {
                stream.fill(&mut mix);
                for (bytes, x) in raw.as_chunks_mut::<4>().0.iter_mut().zip(&mix) {
                    bytes.copy_from_slice(&x.to_le_bytes());
                }
                if let Err(e) = pa.stream.write_copy(&raw, 0, SeekMode::Relative) {
                    stream.failed(format!("stopped: {device}: {}", explain(e)));
                    return;
                }
                last_write = Instant::now();
            }
            if last_write.elapsed() > STALL_TIMEOUT {
                stream.failed(format!("stopped: {device} stopped playing (unplugged?)"));
                break;
            }
        }
    }
}
