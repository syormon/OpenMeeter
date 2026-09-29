//! WASAPI device access for the shared mixing engine
//! ([`openmeeter_backend::engine`]). Shared-mode autoconversion gives every stream
//! the engine format (48 kHz stereo f32). Audio threads run in the MMCSS
//! "Pro Audio" class so the scheduler doesn't starve them.

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
    let mode = wasapi::StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: default_period * 2 };
    client.initialize_client(&format, &direction, &mode).map_err(|e| err(&name, e))?;
    let event = client.set_get_eventhandle().map_err(|e| err(&name, e))?;
    let info = StreamInfo {
        sample_rate: device_format.get_samplespersec(),
        channels: device_format.get_nchannels(),
        bits: device_format.get_validbitspersample(),
        buffer_frames: client.get_buffer_size().map_err(|e| err(&name, e))?,
    };
    Ok((client, event, info))
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
    let setup = || {
        let (client, event, info) = open_client(device_id, wasapi::Direction::Render)?;
        let render = client.get_audiorenderclient().map_err(|e| e.to_string())?;
        let frames = client.get_buffer_size().map_err(|e| e.to_string())? as usize;
        // Start from a buffer of silence, as WASAPI recommends.
        let space = client.get_available_space_in_frames().map_err(|e| e.to_string())? as usize;
        render.write_to_device(space, &vec![0u8; space * BYTES_PER_FRAME], None).map_err(|e| e.to_string())?;
        client.start_stream().map_err(|e| e.to_string())?;
        Ok::<_, String>((client, event, render, frames, info))
    };
    let (client, event, render, max_frames, info) = match setup() {
        Ok(v) => v,
        Err(e) => return stream.failed(e),
    };
    stream.started(info, max_frames);

    let mut mix = vec![0f32; max_frames * CHANNELS];
    let mut raw = vec![0u8; max_frames * BYTES_PER_FRAME];
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
        for (bytes, x) in raw.as_chunks_mut::<4>().0.iter_mut().zip(mix.iter()) {
            bytes.copy_from_slice(&x.to_le_bytes());
        }
        if let Err(e) = render.write_to_device(frames, &raw[..frames * BYTES_PER_FRAME], None) {
            stream.failed(format!("stopped: {e}"));
            break;
        }
    }
    let _ = client.stop_stream();
}
