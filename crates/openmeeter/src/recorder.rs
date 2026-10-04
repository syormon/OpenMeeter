//! The cassette recorder: records armed inputs or buses to WAV and plays audio
//! files into the mix. Decoding and file writing run on their own threads; the
//! backend only exposes a [`Player`] and a [`RecordingStream`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use openmeeter_backend::{AudioBackend, ENGINE_CHANNELS, ENGINE_SAMPLE_RATE, Player, RecordingStream};

use crate::model::{RecordFormat, RecorderSettings};

/// Fast-forward / rewind step.
const SKIP_SECONDS: u64 = 10;

pub struct LoadedFile {
    pub name: String,
    /// Sample rate and channel count of the file before conversion.
    pub source_rate: u32,
    pub source_channels: usize,
}

struct ActiveRecording {
    /// File name shown while recording (the base name for multitrack).
    name: String,
    /// What's being recorded, e.g. "Input #1 + A1".
    sources: String,
    frames: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    writer: JoinHandle<Result<(), String>>,
}

type Decoded = Result<(LoadedFile, Vec<f32>), String>;

/// What the recorder panel shows.
pub struct Display {
    pub title: String,
    pub detail: String,
    pub time: String,
    pub recording: bool,
    pub playing: bool,
    pub error: bool,
}

pub struct Recorder {
    player: Option<Arc<Player>>,
    loaded: Option<LoadedFile>,
    /// Where the loaded file came from, so a macro replaying it skips decoding.
    loaded_path: Option<PathBuf>,
    loading: Option<(String, JoinHandle<Decoded>)>,
    /// Path being decoded, and whether to play it once decoded regardless of the
    /// play-on-load setting (a macro's sound clip).
    loading_path: Option<(PathBuf, bool)>,
    recording: Option<ActiveRecording>,
    /// Last saved file or error, shown on the display.
    message: Option<(String, bool)>,
}

impl Recorder {
    pub fn new(player: Option<Arc<Player>>) -> Self {
        Self { player, loaded: None, loaded_path: None, loading: None, loading_path: None, recording: None, message: None }
    }

    /// Finish background work (a decoded file, a failed writer), apply the loop
    /// setting, and stop a recording that reached its time limit.
    pub fn poll(&mut self, backend: &mut dyn AudioBackend, settings: &RecorderSettings) {
        if let Some(player) = &self.player {
            player.set_looping(settings.loop_playback);
        }
        if self.loading.as_ref().is_some_and(|(_, t)| t.is_finished()) {
            let (name, thread) = self.loading.take().expect("checked above");
            let (path, force_play) = self.loading_path.take().unzip();
            match thread.join().unwrap_or_else(|_| Err("decoder crashed".into())) {
                Ok((file, samples)) => {
                    if let Some(player) = &self.player {
                        player.load(samples.into());
                        if settings.play_on_load || force_play == Some(true) {
                            player.play();
                        }
                    }
                    self.loaded = Some(file);
                    self.loaded_path = path;
                    self.message = None;
                }
                Err(e) => self.message = Some((format!("Couldn't load {name}: {e}"), true)),
            }
        }
        if let Some(rec) = &self.recording {
            if rec.writer.is_finished() {
                // The writer only exits early on an error.
                self.stop_recording(backend);
            } else if let Some(minutes) = settings.stop_after_minutes {
                let limit = u64::from(minutes) * 60 * u64::from(ENGINE_SAMPLE_RATE);
                if rec.frames.load(Ordering::Relaxed) >= limit {
                    self.stop_recording(backend);
                }
            }
        }
    }

    pub fn has_file(&self) -> bool {
        self.loaded.is_some()
    }

    /// Stop playback and release the loaded file.
    pub fn eject(&mut self) {
        if let Some(player) = &self.player {
            player.unload();
        }
        self.loaded = None;
        self.loaded_path = None;
        self.message = None;
    }

    pub fn load(&mut self, path: PathBuf) {
        self.start_loading(path, false);
    }

    /// Play `path` from the start: at once if it's the loaded file, else after
    /// decoding it (whatever the play-on-load setting says).
    pub fn play_file(&mut self, path: PathBuf) {
        if self.loaded_path.as_ref() == Some(&path)
            && let Some(player) = &self.player
        {
            player.seek(0);
            player.play();
            return;
        }
        self.start_loading(path, true);
    }

    fn start_loading(&mut self, path: PathBuf, play: bool) {
        if self.player.is_none() {
            self.message = Some(("This audio backend can't play files yet".into(), true));
            return;
        }
        let name = file_name(&path);
        self.loading_path = Some((path.clone(), play));
        let thread = std::thread::spawn(move || decode_file(&path));
        self.loading = Some((name, thread));
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Stop playback and rewind, leaving any recording running.
    pub fn stop_playback(&mut self) {
        if let Some(player) = &self.player {
            player.stop();
        }
    }

    pub fn play_pause(&mut self) {
        if let Some(player) = &self.player {
            if player.is_playing() { player.pause() } else { player.play() }
        }
    }

    /// Stop playback and rewind; also ends a recording.
    pub fn stop(&mut self, backend: &mut dyn AudioBackend) {
        if let Some(player) = &self.player {
            player.stop();
        }
        self.stop_recording(backend);
    }

    pub fn skip(&mut self, forward: bool) {
        if let Some(player) = &self.player {
            let step = SKIP_SECONDS * ENGINE_SAMPLE_RATE as u64;
            let pos = player.position();
            player.seek(if forward { pos + step } else { pos.saturating_sub(step) });
        }
    }

    /// `labels` maps node keys to display names ("hw1" -> "Input #1").
    pub fn toggle_record(&mut self, backend: &mut dyn AudioBackend, settings: &RecorderSettings, labels: &HashMap<String, String>) {
        if self.recording.is_some() {
            self.stop_recording(backend);
        } else if let Err(e) = self.start_recording(backend, settings, labels) {
            self.message = Some((e, true));
        }
    }

    fn start_recording(&mut self, backend: &mut dyn AudioBackend, settings: &RecorderSettings, labels: &HashMap<String, String>) -> Result<(), String> {
        let keys: Vec<String> = settings.armed().iter().cloned().collect();
        if keys.is_empty() {
            return Err("Arm inputs or buses to record (right-click the recorder)".into());
        }
        let stream = backend.start_recording(&keys).map_err(|e| e.to_string())?;
        let folder = recordings_folder(settings);
        std::fs::create_dir_all(&folder).map_err(|e| format!("Couldn't create {}: {e}", folder.display()))?;
        let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
        let prefix = settings.prefix.trim();
        let base = if prefix.is_empty() { stamp } else { format!("{prefix} {stamp}") };
        let label = |key: &String| labels.get(key).cloned().unwrap_or_else(|| key.clone());
        let sources = keys.iter().map(label).collect::<Vec<_>>().join(" + ");

        let target = WavTarget {
            folder,
            base: base.clone(),
            format: settings.format,
            channels: settings.channels.clamp(1, 2),
            multitrack: settings.multitrack,
            labels: keys.iter().map(|k| (k.clone(), label(k))).collect(),
        };
        let frames = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (frames, stop) = (frames.clone(), stop.clone());
            std::thread::Builder::new()
                .name("recorder-writer".into())
                .spawn(move || target.write(stream, &stop, &frames))
                .map_err(|e| e.to_string())?
        };
        let name = if settings.multitrack { format!("{base} (multitrack)") } else { format!("{base}.wav") };
        self.recording = Some(ActiveRecording { name, sources, frames, stop, writer });
        self.message = None;
        Ok(())
    }

    /// Finish the file(s) and report where they went. Safe to call when not recording.
    pub fn stop_recording(&mut self, backend: &mut dyn AudioBackend) {
        let Some(rec) = self.recording.take() else { return };
        rec.stop.store(true, Ordering::Relaxed);
        backend.stop_recording();
        self.message = Some(match rec.writer.join().unwrap_or_else(|_| Err("writer crashed".into())) {
            Ok(()) => (format!("Saved {}", rec.name), false),
            Err(e) => (format!("Recording failed: {e}"), true),
        });
    }

    pub fn display(&self, settings: &RecorderSettings) -> Display {
        let playing = self.player.as_ref().is_some_and(|p| p.is_playing());
        if let Some(rec) = &self.recording {
            let seconds = rec.frames.load(Ordering::Relaxed) / ENGINE_SAMPLE_RATE as u64;
            return Display {
                title: format!("REC  {}", rec.name),
                detail: format!("{} - {}", rec.sources, settings.format.label()),
                time: clock(seconds),
                recording: true,
                playing,
                error: false,
            };
        }
        if let Some((name, _)) = &self.loading {
            return Display { title: name.clone(), detail: "Loading...".into(), time: clock(0), recording: false, playing, error: false };
        }

        let (pos, len) = self.player.as_ref().map_or((0, 0), |p| (p.position(), p.length()));
        let rate = ENGINE_SAMPLE_RATE as u64;
        let time = if len > 0 { format!("{} / {}", clock(pos / rate), clock(len / rate)) } else { clock(0) };
        let (title, mut detail) = match &self.loaded {
            Some(f) => (f.name.clone(), format!("{} Hz - {} ch", f.source_rate, f.source_channels)),
            None => ("No file loaded".to_string(), "Click here to load a file".to_string()),
        };
        let mut error = false;
        if let Some((msg, is_error)) = &self.message {
            detail = msg.clone();
            error = *is_error;
        }
        Display { title, detail, time, recording: false, playing, error }
    }
}

pub fn recordings_folder(settings: &RecorderSettings) -> PathBuf {
    settings.folder.clone().unwrap_or_else(|| {
        let docs = directories::UserDirs::new().and_then(|d| d.document_dir().map(Path::to_path_buf));
        docs.unwrap_or_else(|| PathBuf::from(".")).join("OpenMeeter")
    })
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn clock(seconds: u64) -> String {
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// Where and how a recording is written.
struct WavTarget {
    folder: PathBuf,
    base: String,
    format: RecordFormat,
    /// 1 = mono (L/R averaged) or 2.
    channels: u16,
    /// One file per armed source instead of one mixed file.
    multitrack: bool,
    /// Node key -> label, used to name multitrack files.
    labels: HashMap<String, String>,
}

impl WavTarget {
    /// Pull the stream into WAV file(s) until `stop` is set, then drain and
    /// finalise. Headers are refreshed every second so a crash loses little.
    fn write(&self, mut stream: Box<dyn RecordingStream>, stop: &AtomicBool, frames: &AtomicU64) -> Result<(), String> {
        let mut single = None;
        let mut tracks: Vec<(String, WavFile)> = Vec::new();
        if !self.multitrack {
            single = Some(WavFile::create(&self.folder.join(format!("{}.wav", self.base)), self.format, self.channels)?);
        }
        let mut buf = Vec::new();
        loop {
            let n = if let Some(file) = single.as_mut() {
                buf.clear();
                let n = stream.read(&mut buf);
                file.write(&buf)?;
                n
            } else {
                let mut n = 0;
                for (key, samples) in stream.read_tracks() {
                    let file = match tracks.iter_mut().position(|(k, _)| *k == key) {
                        Some(i) => &mut tracks[i].1,
                        None => {
                            let label = self.labels.get(&key).cloned().unwrap_or_else(|| key.clone());
                            let path = self.folder.join(format!("{} - {}.wav", self.base, sanitize(&label)));
                            tracks.push((key, WavFile::create(&path, self.format, self.channels)?));
                            &mut tracks.last_mut().expect("just pushed").1
                        }
                    };
                    file.write(&samples)?;
                    n = n.max(samples.len() / ENGINE_CHANNELS);
                }
                n
            };
            frames.fetch_add(n as u64, Ordering::Relaxed);
            if n == 0 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        for file in single.into_iter().chain(tracks.into_iter().map(|(_, f)| f)) {
            file.finalize()?;
        }
        Ok(())
    }
}

/// Keep file names valid on every OS.
fn sanitize(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c }).collect()
}

/// One WAV file being written from engine-format (stereo f32) audio.
struct WavFile {
    wav: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    format: RecordFormat,
    mono: bool,
    since_flush: u64,
}

impl WavFile {
    fn create(path: &Path, format: RecordFormat, channels: u16) -> Result<Self, String> {
        let (bits, sample_format) = match format {
            RecordFormat::Pcm16 => (16, hound::SampleFormat::Int),
            RecordFormat::Pcm24 => (24, hound::SampleFormat::Int),
            RecordFormat::Float32 => (32, hound::SampleFormat::Float),
        };
        let spec = hound::WavSpec { channels, sample_rate: ENGINE_SAMPLE_RATE, bits_per_sample: bits, sample_format };
        let wav = hound::WavWriter::create(path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self { wav, format, mono: channels == 1, since_flush: 0 })
    }

    fn write(&mut self, stereo: &[f32]) -> Result<(), String> {
        let mut put = |x: f32| {
            let x = x.clamp(-1.0, 1.0);
            match self.format {
                RecordFormat::Pcm16 => self.wav.write_sample((x * i16::MAX as f32) as i16),
                RecordFormat::Pcm24 => self.wav.write_sample((x * 8_388_607.0) as i32),
                RecordFormat::Float32 => self.wav.write_sample(x),
            }
        };
        for frame in stereo.as_chunks::<ENGINE_CHANNELS>().0 {
            if self.mono {
                put((frame[0] + frame[1]) * 0.5)
            } else {
                put(frame[0]).and_then(|()| put(frame[1]))
            }
            .map_err(|e| e.to_string())?;
        }
        self.since_flush += (stereo.len() / ENGINE_CHANNELS) as u64;
        if self.since_flush >= ENGINE_SAMPLE_RATE as u64 {
            self.wav.flush().map_err(|e| e.to_string())?;
            self.since_flush = 0;
        }
        Ok(())
    }

    fn finalize(self) -> Result<(), String> {
        self.wav.finalize().map_err(|e| e.to_string())
    }
}

/// Decode any supported file to the engine format (48 kHz stereo f32).
fn decode_file(path: &Path) -> Decoded {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("unsupported file ({e})"))?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio track")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("unsupported codec ({e})"))?;
    let track_id = track.id;

    let (mut rate, mut channels) = (0u32, 0usize);
    let mut samples: Vec<f32> = Vec::new();
    let mut packet_buf: Vec<f32> = Vec::new();
    while let Some(packet) = format.next_packet().map_err(|e| e.to_string())? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                rate = audio.spec().rate();
                channels = audio.spec().channels().count();
                packet_buf.resize(audio.samples_interleaved(), 0.0);
                audio.copy_to_slice_interleaved(&mut packet_buf);
                samples.extend_from_slice(&packet_buf);
            }
            Err(Error::DecodeError(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if channels == 0 || samples.is_empty() {
        return Err("no audio decoded".into());
    }

    let stereo = to_stereo(&samples, channels);
    let converted = resample(stereo, rate)?;
    let file = LoadedFile { name: file_name(path), source_rate: rate, source_channels: channels };
    Ok((file, converted))
}

/// Mono is duplicated to both sides; extra channels beyond front L/R are dropped.
fn to_stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        2 => samples.to_vec(),
        1 => samples.iter().flat_map(|&x| [x, x]).collect(),
        n => samples.chunks_exact(n).flat_map(|f| [f[0], f[1]]).collect(),
    }
}

/// High-quality offline conversion of an interleaved stereo clip to 48 kHz.
fn resample(stereo: Vec<f32>, rate: u32) -> Result<Vec<f32>, String> {
    use rubato::audioadapter_buffers::owned::InterleavedOwned;
    use rubato::{Fft, FixedSync, Resampler};

    if rate == ENGINE_SAMPLE_RATE {
        return Ok(stereo);
    }
    let frames = stereo.len() / ENGINE_CHANNELS;
    let input = InterleavedOwned::new_from(stereo, ENGINE_CHANNELS, frames).map_err(|e| e.to_string())?;
    let mut resampler = Fft::<f32>::new(rate as usize, ENGINE_SAMPLE_RATE as usize, 1024, ENGINE_CHANNELS, FixedSync::Input)
        .map_err(|e| e.to_string())?;
    let output = resampler.process_all(&input, frames, None).map_err(|e| e.to_string())?;
    Ok(output.take_data())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_and_surround_become_stereo() {
        assert_eq!(to_stereo(&[0.1, 0.2], 1), [0.1, 0.1, 0.2, 0.2]);
        assert_eq!(to_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3), [1.0, 2.0, 4.0, 5.0]);
    }

    #[test]
    fn resampling_keeps_duration() {
        // One second of a 1 kHz tone at 44.1 kHz becomes one second at 48 kHz.
        let src: Vec<f32> = (0..44_100).flat_map(|i| {
            let x = (i as f32 * 1000.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.5;
            [x, x]
        }).collect();
        let out = resample(src, 44_100).unwrap();
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() < 50, "got {frames} frames");
        let peak = out.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.05, "amplitude preserved, got {peak}");
    }

    /// Two sources: "hw1" at 0.25 and "A1" at -0.5, `left` frames each.
    struct TwoTracks {
        left: usize,
    }

    impl TwoTracks {
        fn take(&mut self) -> usize {
            let n = self.left.min(4800);
            self.left -= n;
            n
        }
    }

    impl RecordingStream for TwoTracks {
        fn read(&mut self, out: &mut Vec<f32>) -> usize {
            let n = self.take();
            out.extend(std::iter::repeat_n(-0.25, n * 2));
            n
        }

        fn read_tracks(&mut self) -> Vec<(String, Vec<f32>)> {
            let n = self.take();
            vec![("hw1".into(), vec![0.25; n * 2]), ("A1".into(), vec![-0.5; n * 2])]
        }
    }

    fn target(dir: &Path, format: RecordFormat, channels: u16, multitrack: bool) -> WavTarget {
        let labels = HashMap::from([("hw1".to_string(), "Input #1".to_string()), ("A1".to_string(), "A1".to_string())]);
        WavTarget { folder: dir.to_path_buf(), base: format!("{format:?}-{channels}-{multitrack}"), format, channels, multitrack, labels }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openmeeter-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_a_valid_wav_in_every_format() {
        let dir = temp_dir("wav");
        for format in RecordFormat::ALL {
            let stop = AtomicBool::new(true); // drain what's there, then finish
            let frames = AtomicU64::new(0);
            let t = target(&dir, format, 2, false);
            t.write(Box::new(TwoTracks { left: 12_000 }), &stop, &frames).unwrap();
            let reader = hound::WavReader::open(dir.join(format!("{}.wav", t.base))).unwrap();
            assert_eq!(reader.spec().sample_rate, 48_000);
            assert_eq!(reader.spec().channels, 2);
            assert_eq!(reader.duration(), 12_000, "{format:?}");
            assert_eq!(frames.load(Ordering::Relaxed), 12_000);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multitrack_writes_one_mono_file_per_source() {
        let dir = temp_dir("multi");
        let (stop, frames) = (AtomicBool::new(true), AtomicU64::new(0));
        let t = target(&dir, RecordFormat::Float32, 1, true);
        t.write(Box::new(TwoTracks { left: 9_600 }), &stop, &frames).unwrap();
        for (label, level) in [("Input #1", 0.25), ("A1", -0.5)] {
            let mut reader = hound::WavReader::open(dir.join(format!("{} - {label}.wav", t.base))).unwrap();
            assert_eq!(reader.spec().channels, 1);
            assert_eq!(reader.duration(), 9_600);
            let first: f32 = reader.samples::<f32>().next().unwrap().unwrap();
            assert_eq!(first, level, "{label} is its own track");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
