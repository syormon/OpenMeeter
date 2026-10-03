//! WASAPI device access for the shared mixing engine
//! ([`openmeeter_backend::engine`]). Shared-mode autoconversion gives every stream
//! the engine format (48 kHz stereo f32). Outputs can instead be opened in
//! exclusive mode, bypassing the Windows mixer and its effects: the device then
//! gets 48 kHz stereo in the best sample format it accepts. Audio threads run in
//! the MMCSS "Pro Audio" class so the scheduler doesn't starve them.

use openmeeter_backend::StreamInfo;
use openmeeter_backend::engine::{CHANNELS, CaptureStream, DeviceIo, RenderStream, SAMPLE_RATE};

const BYTES_PER_FRAME: usize = CHANNELS * 4;
const EVENT_TIMEOUT_MS: u32 = 200;

pub struct Wasapi;

impl DeviceIo for Wasapi {
    fn capture(&self, device: &str, stream: CaptureStream) {
        capture(device, stream);
    }

    fn render(&self, device: &str, stream: RenderStream) {
        render(device, stream);
    }
}

fn open_client(device_id: &str, direction: wasapi::Direction) -> Result<(wasapi::AudioClient, wasapi::Handle, StreamInfo), String> {
    let err = |what: &str, e: wasapi::WasapiError| format!("{what}: {}", explain(&e));
    let hr = wasapi::initialize_mta();
    if hr.is_err() {
        return Err(format!("COM init failed: {hr:?}"));
    }
    let device = wasapi::DeviceEnumerator::new()
        .and_then(|e| e.get_device(device_id))
        .map_err(|e| err("device not found", e))?;
    let name = device.get_friendlyname().unwrap_or_else(|_| device_id.to_string());
    let mut client = device.get_iaudioclient().map_err(|e| err(&name, e))?;
    let device_format = client.get_mixformat().map_err(|e| err(&name, e))?;
    let format = wasapi::WaveFormat::new(32, 32, &wasapi::SampleType::Float, SAMPLE_RATE, CHANNELS, None);
    let (default_period, _) = client.get_device_period().map_err(|e| err(&name, e))?;
    if direction == wasapi::Direction::Render {
        // Raw mode skips the device's effects (Realtek "enhancements", virtual surround,
        // loudness...), which otherwise reshape everything we play. Devices without raw
        // support refuse it; they play with effects as before.
        let raw = wasapi::AudioClientProperties::new().set_option(windows::Win32::Media::Audio::AUDCLNT_STREAMOPTIONS_RAW);
        if let Err(e) = client.set_properties(raw) {
            log::info!("{name}: raw mode unavailable, device effects stay on ({e})");
        }
    }
    let mode = wasapi::StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: default_period * 2 };
    client.initialize_client(&format, &direction, &mode).map_err(|e| err(&name, e))?;
    let event = client.set_get_eventhandle().map_err(|e| err(&name, e))?;
    let info = StreamInfo {
        sample_rate: device_format.get_samplespersec(),
        channels: device_format.get_nchannels(),
        bits: device_format.get_validbitspersample(),
        buffer_frames: client.get_buffer_size().map_err(|e| err(&name, e))?,
        exclusive: false,
    };
    Ok((client, event, info))
}

/// A sample format an exclusive-mode device may accept, best first.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DeviceSample {
    F32,
    /// 32-bit container; `valid` is 24 or 32.
    I32 { valid: u16 },
    I24,
    I16,
}

impl DeviceSample {
    const PREFERENCE: [DeviceSample; 5] =
        [DeviceSample::F32, DeviceSample::I32 { valid: 24 }, DeviceSample::I32 { valid: 32 }, DeviceSample::I24, DeviceSample::I16];

    fn wave_format(self) -> wasapi::WaveFormat {
        let (store, valid, kind) = match self {
            DeviceSample::F32 => (32, 32, wasapi::SampleType::Float),
            DeviceSample::I32 { valid } => (32, valid as usize, wasapi::SampleType::Int),
            DeviceSample::I24 => (24, 24, wasapi::SampleType::Int),
            DeviceSample::I16 => (16, 16, wasapi::SampleType::Int),
        };
        wasapi::WaveFormat::new(store, valid, &kind, SAMPLE_RATE, CHANNELS, None)
    }

    fn bytes(self) -> usize {
        match self {
            DeviceSample::F32 | DeviceSample::I32 { .. } => 4,
            DeviceSample::I24 => 3,
            DeviceSample::I16 => 2,
        }
    }

    fn valid_bits(self) -> u16 {
        match self {
            DeviceSample::F32 => 32,
            DeviceSample::I32 { valid } => valid,
            DeviceSample::I24 => 24,
            DeviceSample::I16 => 16,
        }
    }
}

/// Converts engine samples (f32) to a device format. 16-bit output gets TPDF
/// dither, which turns truncation distortion into a faint, even noise floor.
struct Encoder {
    sample: DeviceSample,
    rng: u32,
}

impl Encoder {
    fn new(sample: DeviceSample) -> Self {
        Self { sample, rng: 0x9E37_79B9 }
    }

    /// Uniform random value in [-0.5, 0.5) (xorshift32).
    fn noise(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) - 0.5
    }

    fn encode(&mut self, samples: &[f32], out: &mut [u8]) {
        let width = self.sample.bytes();
        for (x, bytes) in samples.iter().zip(out.chunks_exact_mut(width)) {
            let x = x.clamp(-1.0, 1.0);
            match self.sample {
                DeviceSample::F32 => bytes.copy_from_slice(&x.to_le_bytes()),
                DeviceSample::I32 { .. } => bytes.copy_from_slice(&((x as f64 * i32::MAX as f64) as i32).to_le_bytes()),
                DeviceSample::I24 => bytes.copy_from_slice(&((x * 8_388_607.0) as i32).to_le_bytes()[..3]),
                DeviceSample::I16 => {
                    let dithered = x * 32_767.0 + self.noise() + self.noise();
                    bytes.copy_from_slice(&(dithered.round().clamp(-32_768.0, 32_767.0) as i16).to_le_bytes());
                }
            }
        }
    }
}

/// Open a render device exclusively at 48 kHz stereo in the best sample format
/// it accepts, with an event-driven buffer of about one device period.
fn open_exclusive(device_id: &str) -> Result<(wasapi::AudioClient, wasapi::Handle, StreamInfo, DeviceSample), String> {
    const BUFFER_SIZE_NOT_ALIGNED: i32 = 0x8889_0019_u32 as i32;
    let err = |what: &str, e: wasapi::WasapiError| format!("{what}: {}", explain(&e));
    let hr = wasapi::initialize_mta();
    if hr.is_err() {
        return Err(format!("COM init failed: {hr:?}"));
    }
    let device = wasapi::DeviceEnumerator::new()
        .and_then(|e| e.get_device(device_id))
        .map_err(|e| err("device not found", e))?;
    let name = device.get_friendlyname().unwrap_or_else(|_| device_id.to_string());
    let mut client = device.get_iaudioclient().map_err(|e| err(&name, e))?;

    let (sample, format) = DeviceSample::PREFERENCE
        .iter()
        .find_map(|&s| client.is_supported_exclusive_with_quirks(&s.wave_format()).ok().map(|f| (s, f)))
        .ok_or_else(|| format!("{name}: doesn't support 48 kHz stereo in exclusive mode (turn exclusive mode off for this output)"))?;
    let (default_period, _) = client.get_device_period().map_err(|e| err(&name, e))?;
    // Some drivers (e.g. Intel HDA) need the buffer aligned to 128 bytes.
    let period = client.calculate_aligned_period_near(default_period, Some(128), &format).map_err(|e| err(&name, e))?;
    let mode = wasapi::StreamMode::EventsExclusive { period_hns: period };
    if let Err(e) = client.initialize_client(&format, &wasapi::Direction::Render, &mode) {
        let unaligned = matches!(&e, wasapi::WasapiError::Windows(w) if w.code().0 == BUFFER_SIZE_NOT_ALIGNED);
        if !unaligned {
            return Err(err(&name, e));
        }
        // Documented recovery: retry with the aligned size the driver suggests, on a new client.
        let frames = client.get_buffer_size().map_err(|e| err(&name, e))?;
        let period = wasapi::calculate_period_100ns(frames as i64, SAMPLE_RATE as i64);
        client = device.get_iaudioclient().map_err(|e| err(&name, e))?;
        let mode = wasapi::StreamMode::EventsExclusive { period_hns: period };
        client.initialize_client(&format, &wasapi::Direction::Render, &mode).map_err(|e| err(&name, e))?;
    }
    let event = client.set_get_eventhandle().map_err(|e| err(&name, e))?;
    let info = StreamInfo {
        sample_rate: SAMPLE_RATE as u32,
        channels: CHANNELS as u16,
        bits: sample.valid_bits(),
        buffer_frames: client.get_buffer_size().map_err(|e| err(&name, e))?,
        exclusive: true,
    };
    log::info!("{name}: exclusive mode, {sample:?}, {} frame buffer", info.buffer_frames);
    Ok((client, event, info, sample))
}

/// Puts the current thread in the MMCSS "Pro Audio" class until dropped.
struct ProAudioPriority(Option<windows::Win32::Foundation::HANDLE>);

impl ProAudioPriority {
    fn enter() -> Self {
        let mut task_index = 0u32;
        // SAFETY: valid null-terminated task name and out-pointer; the handle is reverted in Drop.
        let handle = unsafe {
            windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task_index)
        };
        if let Err(e) = &handle {
            log::warn!("could not raise audio thread priority: {e}");
        }
        Self(handle.ok())
    }
}

impl Drop for ProAudioPriority {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            // SAFETY: handle came from AvSetMmThreadCharacteristicsW on this thread.
            let _ = unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

/// Plain-English text for the WASAPI failures people actually hit.
fn explain(e: &wasapi::WasapiError) -> String {
    let raw = e.to_string();
    let known = [
        ("0x8889000A", "in use by another app in exclusive mode (e.g. Voicemeeter or a DAW). Close it or disable exclusive mode in the device's Windows sound settings."),
        ("0x88890004", "the device was unplugged or disabled."),
        ("0x88890008", "the device doesn't support the requested format."),
        (
            "0x8889000E",
            "exclusive mode is turned off for this device. In Windows Sound settings, open the device's Properties > Advanced and tick \"Allow applications to take exclusive control\", or turn exclusive mode off here.",
        ),
        ("0x80070490", "the device wasn't found."),
    ];
    match known.iter().find(|(code, _)| raw.contains(code)) {
        Some((code, text)) => format!("{text} ({code})"),
        None => raw,
    }
}

fn capture(device_id: &str, mut stream: CaptureStream) {
    let _priority = ProAudioPriority::enter();
    let setup = || {
        let (client, event, info) = open_client(device_id, wasapi::Direction::Capture)?;
        let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
        let frames = client.get_buffer_size().map_err(|e| e.to_string())? as usize;
        client.start_stream().map_err(|e| e.to_string())?;
        Ok::<_, String>((client, event, capture, frames, info))
    };
    let (client, event, capture, max_frames, info) = match setup() {
        Ok(v) => v,
        Err(e) => return stream.failed(e),
    };
    stream.started(info);

    let mut raw = vec![0u8; max_frames * BYTES_PER_FRAME];
    let mut samples = vec![0f32; max_frames * CHANNELS];
    'run: while stream.is_running() {
        if event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
            continue;
        }
        loop {
            let frames = match capture.get_next_packet_size() {
                Ok(Some(n)) if n > 0 => n as usize,
                Ok(_) => break,
                Err(e) => {
                    stream.failed(format!("stopped: {e}"));
                    break 'run;
                }
            };
            if frames > max_frames {
                raw.resize(frames * BYTES_PER_FRAME, 0);
                samples.resize(frames * CHANNELS, 0.0);
            }
            let (read, info) = match capture.read_from_device(&mut raw[..frames * BYTES_PER_FRAME]) {
                Ok(v) => (v.0 as usize, v.1),
                Err(e) => {
                    stream.failed(format!("stopped: {e}"));
                    break 'run;
                }
            };
            let buf = &mut samples[..read * CHANNELS];
            if info.flags.silent {
                buf.fill(0.0);
            } else {
                for (out, bytes) in buf.iter_mut().zip(raw.as_chunks::<4>().0) {
                    *out = f32::from_le_bytes(*bytes);
                }
            }
            stream.push(buf);
        }
    }
    let _ = client.stop_stream();
}

fn render(device_id: &str, mut stream: RenderStream) {
    let _priority = ProAudioPriority::enter();
    let exclusive = stream.exclusive();
    let setup = || {
        let (client, event, info, sample) = if exclusive {
            open_exclusive(device_id)?
        } else {
            let (client, event, info) = open_client(device_id, wasapi::Direction::Render)?;
            (client, event, info, DeviceSample::F32)
        };
        let render = client.get_audiorenderclient().map_err(|e| e.to_string())?;
        let frames = client.get_buffer_size().map_err(|e| e.to_string())? as usize;
        // Start from a buffer of silence, as WASAPI recommends (and exclusive mode requires).
        let space = client.get_available_space_in_frames().map_err(|e| e.to_string())? as usize;
        let frame_bytes = CHANNELS * sample.bytes();
        render.write_to_device(space, &vec![0u8; space * frame_bytes], None).map_err(|e| e.to_string())?;
        client.start_stream().map_err(|e| e.to_string())?;
        Ok::<_, String>((client, event, render, frames, info, sample))
    };
    let (client, event, render, max_frames, info, sample) = match setup() {
        Ok(v) => v,
        Err(e) => return stream.failed(e),
    };
    stream.started(info, max_frames);

    let frame_bytes = CHANNELS * sample.bytes();
    let mut encoder = Encoder::new(sample);
    let mut mix = vec![0f32; max_frames * CHANNELS];
    let mut raw = vec![0u8; max_frames * frame_bytes];
    while stream.is_running() {
        if event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
            continue;
        }
        let frames = match client.get_available_space_in_frames() {
            Ok(n) => (n as usize).min(max_frames),
            Err(e) => {
                stream.failed(format!("stopped: {e}"));
                break;
            }
        };
        if frames == 0 {
            continue;
        }
        let mix = &mut mix[..frames * CHANNELS];
        stream.fill(mix);
        encoder.encode(mix, &mut raw[..frames * frame_bytes]);
        if let Err(e) = render.write_to_device(frames, &raw[..frames * frame_bytes], None) {
            stream.failed(format!("stopped: {e}"));
            break;
        }
    }
    let _ = client.stop_stream();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(sample: DeviceSample, x: &[f32]) -> Vec<u8> {
        let mut out = vec![0u8; x.len() * sample.bytes()];
        Encoder::new(sample).encode(x, &mut out);
        out
    }

    #[test]
    fn float_is_bit_exact() {
        let x = [0.123_456_78f32, -1.0, 1.0];
        let out = encode(DeviceSample::F32, &x);
        let back: Vec<f32> = out.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        assert_eq!(back, x);
    }

    #[test]
    fn integer_formats_scale_and_clip() {
        let out = encode(DeviceSample::I24, &[1.0, -1.0, 0.5, 2.0]);
        let v: Vec<i32> = out.as_chunks::<3>().0.iter().map(|b| i32::from_le_bytes([b[0], b[1], b[2], if b[2] & 0x80 != 0 { 0xFF } else { 0 }])).collect();
        assert_eq!(v, [8_388_607, -8_388_607, 4_194_303, 8_388_607], "2.0 clips to full scale");

        let out = encode(DeviceSample::I32 { valid: 24 }, &[1.0, -0.5]);
        let v: Vec<i32> = out.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b)).collect();
        assert_eq!(v[0], i32::MAX);
        assert!((v[1] as f64 / i32::MAX as f64 + 0.5).abs() < 1e-6);
    }

    #[test]
    fn sixteen_bit_dither_stays_within_one_lsb() {
        let x = vec![0.25f32; 10_000];
        let out = encode(DeviceSample::I16, &x);
        let v: Vec<i16> = out.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        let ideal = 0.25 * 32_767.0;
        assert!(v.iter().all(|&s| (s as f32 - ideal).abs() <= 1.5), "TPDF dither is at most one LSB either way");
        let mean = v.iter().map(|&s| s as f64).sum::<f64>() / v.len() as f64;
        assert!((mean - ideal as f64).abs() < 0.05, "dither is unbiased (mean {mean})");
    }
}
