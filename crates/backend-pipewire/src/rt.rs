//! Real-time scheduling for audio threads, so a busy desktop can't starve them.
//!
//! Tries, in order: SCHED_FIFO directly (works with an `rtprio` limit, e.g. the
//! `audio` group), RealtimeKit over D-Bus (how desktop audio apps normally get
//! it), then a raised nice level. RLIMIT_RTTIME caps how long a real-time
//! thread may run without blocking, so a bug can't freeze the machine; rtkit
//! insists on it too.

use std::process::{Command, Stdio};
use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};

/// Real-time priority for our threads: below PipeWire's own (88 by default),
/// within rtkit's usual maximum of 20.
const RT_PRIORITY: i32 = 10;
/// Fallback when real-time is refused.
const NICE_LEVEL: i32 = -11;
/// CPU time a real-time thread may use without blocking, in microseconds
/// (rtkit's maximum). Our threads block every few milliseconds.
const RTTIME_LIMIT_US: libc::rlim_t = 200_000;

/// How a thread ended up scheduled, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Normal,
    Nice,
    Realtime,
}

/// The worst priority any audio thread got (`u8::MAX` = none started yet).
static WORST: AtomicU8 = AtomicU8::new(u8::MAX);

/// Human-readable scheduling of the audio threads, for stats.
pub fn summary() -> Option<&'static str> {
    match WORST.load(Ordering::Relaxed) {
        0 => Some("audio threads: normal priority (real-time refused; audio may glitch under load)"),
        1 => Some("audio threads: raised priority (real-time refused)"),
        2 => Some("audio threads: real-time"),
        _ => None,
    }
}

/// Raise the calling thread's priority as far as the system allows.
pub fn raise_current_thread() -> Priority {
    static LIMIT: Once = Once::new();
    LIMIT.call_once(limit_rttime);

    // SAFETY: plain syscall wrapper with no arguments.
    let tid = unsafe { libc::gettid() } as u64;
    let priority = if set_fifo() || rtkit("MakeThreadRealtimeWithPID", tid, 'u', RT_PRIORITY) {
        Priority::Realtime
    } else if set_nice(tid) || rtkit("MakeThreadHighPriorityWithPID", tid, 'i', NICE_LEVEL) {
        Priority::Nice
    } else {
        Priority::Normal
    };
    log::debug!("audio thread {tid}: {priority:?} priority");
    WORST.fetch_min(priority as u8, Ordering::Relaxed);
    priority
}

fn limit_rttime() {
    let limit = libc::rlimit { rlim_cur: RTTIME_LIMIT_US, rlim_max: RTTIME_LIMIT_US };
    // SAFETY: valid pointer to an initialised rlimit.
    if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) } != 0 {
        log::debug!("could not limit real-time CPU time: {}", std::io::Error::last_os_error());
    }
}

fn set_fifo() -> bool {
    let param = libc::sched_param { sched_priority: RT_PRIORITY };
    // SCHED_RESET_ON_FORK: child processes (e.g. pactl) don't inherit real-time.
    let policy = libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK;
    // SAFETY: valid pointer to an initialised sched_param; 0 means this thread.
    unsafe { libc::sched_setscheduler(0, policy, &param) == 0 }
}

fn set_nice(tid: u64) -> bool {
    // SAFETY: on Linux, PRIO_PROCESS with a thread ID sets that thread's nice level.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, NICE_LEVEL) == 0 }
}

/// Call a RealtimeKit method taking (pid, tid, `value`), where `kind` is the
/// D-Bus type of `value`.
fn rtkit(method: &str, tid: u64, kind: char, value: i32) -> bool {
    let status = Command::new("busctl")
        .args(["--system", "call", "org.freedesktop.RealtimeKit1", "/org/freedesktop/RealtimeKit1", "org.freedesktop.RealtimeKit1", method])
        .arg(format!("tt{kind}"))
        .arg(std::process::id().to_string())
        .arg(tid.to_string())
        .arg(value.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    status.is_ok_and(|s| s.success())
}
