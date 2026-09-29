use std::time::{Duration, Instant};

use openmeeter_backend::{AudioBackend, Direction};

use crate::config::Config;
use crate::recorder::Recorder;

pub fn devices(mut backend: Box<dyn AudioBackend>) -> anyhow::Result<()> {
    let devices = backend.devices()?;
    for (title, direction) in [("Capture", Direction::Capture), ("Playback", Direction::Playback)] {
        println!("{title}:");
        for d in devices.iter().filter(|d| d.direction == direction) {
            let tag = if d.is_virtual { " [virtual]" } else { "" };
            let default = if d.is_default { " [default]" } else { "" };
            println!("  {} ({}ch){tag}{default}\n    {}", d.name, d.channels, d.id.0);
        }
    }
    Ok(())
}

pub fn doctor(mut backend: Box<dyn AudioBackend>) -> anyhow::Result<()> {
    println!("backend: {}", backend.name());
    let problems = backend.diagnostics();
    if problems.is_empty() {
        println!("no problems found");
    }
    for p in &problems {
        println!("problem: {p}");
    }
    Ok(())
}

pub fn meters(mut backend: Box<dyn AudioBackend>, config: &Config, seconds: u64) -> anyhow::Result<()> {
    if let Err(e) = backend.apply(&config.mixer.to_graph()) {
        println!("routing: {e}");
    }
    let keys: Vec<&str> = config.mixer.strips.iter().map(|s| s.key.as_str()).chain(config.mixer.buses.iter().map(|b| b.key.as_str())).collect();
    println!("{}", keys.iter().map(|k| format!("{k:>7}")).collect::<String>());
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(500));
        let levels = backend.levels();
        let row: String = keys
            .iter()
            .map(|k| {
                let peak = levels.get(*k).map_or(0.0, |p| p.iter().copied().fold(0.0, f32::max));
                if peak > 0.0 { format!("{:>7.1}", 20.0 * peak.log10()) } else { format!("{:>7}", "-") }
            })
            .collect();
        println!("{row}   (dBFS peak)   {}", backend.stats().join(" | "));
    }
    Ok(())
}

pub fn record(mut backend: Box<dyn AudioBackend>, config: &Config, seconds: u64, play: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    if let Err(e) = backend.apply(&config.mixer.to_graph()) {
        println!("routing: {e}");
    }
    let settings = &config.mixer.recorder;
    let mut recorder = Recorder::new(backend.player());
    if let Some(path) = play {
        recorder.load(path);
        // Decoding runs on a thread; wait for it before starting playback.
        let deadline = Instant::now() + Duration::from_secs(30);
        while recorder.is_loading() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            recorder.poll(backend.as_mut(), settings);
        }
        if !recorder.display(settings).playing {
            recorder.play_pause();
        }
    }
    recorder.toggle_record(backend.as_mut(), settings, &config.mixer.node_labels());
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(500));
        recorder.poll(backend.as_mut(), settings);
        let d = recorder.display(settings);
        println!("{}  {}  {}", d.time, d.title, d.detail);
        if !d.recording {
            break;
        }
    }
    recorder.stop(backend.as_mut());
    println!("{}", recorder.display(settings).detail);
    Ok(())
}
