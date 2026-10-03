// Release builds are a GUI app on Windows (no console window on double-click).
// CLI commands still print by attaching to the terminal that launched them.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod config;
mod autostart;
#[cfg(target_os = "linux")]
mod desktop_entry;
mod model;
mod recorder;
mod ui;

use clap::{Parser, Subcommand};
use openmeeter_backend::{AudioBackend, MockBackend};

#[derive(Parser)]
#[command(version, about = "Voicemeeter-style audio mixer for Windows and Linux")]
struct Args {
    /// Use a fake audio backend (for UI development).
    #[arg(long, global = true)]
    mock: bool,

    /// Config file to use instead of the default location.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List audio devices the backend can see.
    Devices,
    /// Check the environment for problems (e.g. VB-Cable missing on Windows).
    Doctor,
    /// Run the mixer from the config without a window, printing meter levels.
    Meters {
        /// How long to run, in seconds.
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Record the recorder's buses without a window (see recorder options in the config).
    Record {
        /// How long to record, in seconds.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Also play this file through the recorder's playback buses while recording.
        #[arg(long, value_name = "FILE")]
        play: Option<std::path::PathBuf>,
    },
    /// List which apps are playing or recording audio on each device (Windows).
    Apps,
    /// Print the config file location.
    ConfigPath,
}

fn platform_backend() -> anyhow::Result<Box<dyn AudioBackend>> {
    #[cfg(windows)]
    return Ok(Box::new(openmeeter_windows::WindowsBackend::new()?));
    #[cfg(target_os = "linux")]
    return Ok(Box::new(openmeeter_pipewire::PipeWireBackend::new()?));
    #[cfg(not(any(windows, target_os = "linux")))]
    anyhow::bail!("unsupported platform; run with --mock");
}

/// Ctrl+C or SIGTERM (e.g. logging out) skip the backend's cleanup, which would
/// leave our virtual devices in the system's device lists; remove them first.
#[cfg(target_os = "linux")]
fn clean_up_on_signal() {
    let result = ctrlc::set_handler(|| {
        openmeeter_pipewire::remove_virtual_devices();
        std::process::exit(130);
    });
    if let Err(e) = result {
        log::warn!("no signal handler: {e}");
    }
}

/// In GUI-subsystem builds, reconnect stdout/stderr to the launching terminal (if
/// any) so `openmeeter devices`, `--help`, errors and logs are visible there.
fn attach_parent_console() {
    #[cfg(all(windows, not(debug_assertions)))]
    // SAFETY: plain Win32 call with a constant argument; failure (no parent console) is harmless.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}

fn main() -> anyhow::Result<()> {
    attach_parent_console();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,openmeeter=info")).init();
    let args = Args::parse();

    let config_path = match args.config {
        Some(path) => path,
        None => config::default_path(args.mock)?,
    };
    let backend = if args.mock { Box::new(MockBackend::new()) } else { platform_backend()? };
    #[cfg(target_os = "linux")]
    if !args.mock {
        clean_up_on_signal();
    }

    match args.command {
        None => ui::run(backend, config_path),
        Some(Command::Devices) => cli::devices(backend),
        Some(Command::Apps) => cli::apps(),
        Some(Command::Doctor) => cli::doctor(backend),
        Some(Command::Record { seconds, play }) => cli::record(backend, &config::load(&config_path), seconds, play),
        Some(Command::Meters { seconds }) => cli::meters(backend, &config::load(&config_path), seconds),
        Some(Command::ConfigPath) => {
            println!("{}", config_path.display());
            Ok(())
        }
    }
}
