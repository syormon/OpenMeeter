//! Logging to the terminal and, for the app itself, to a file beside the config.
//! Release builds on Windows have no console, so without the file an audio
//! glitch would leave nothing to look at afterwards.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The log is restarted (keeping one old copy) once it grows past this.
const MAX_BYTES: u64 = 1024 * 1024;

/// Where the log goes for a given config file.
pub fn path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("openmeeter.log")
}

/// Start logging to stderr, and to `file` too if it can be opened.
pub fn init(file: Option<&Path>) {
    // wgpu_hal relays the Vulkan loader's complaints about other apps' overlay layers.
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,openmeeter=info,wgpu_hal=off"));
    if let Some(file) = file.and_then(open) {
        builder.target(env_logger::Target::Pipe(Box::new(Tee(file))));
    }
    builder.init();
}

fn open(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Writes every log line to stderr and the log file. Neither may fail logging.
struct Tee(File);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        let _ = self.0.write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        self.0.flush()
    }
}
