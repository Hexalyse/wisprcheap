//! Console output teed into `wisprcheap.log` (rotated at 1 MB) and registered sinks (the tray's log window).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

const SESSION_MARKER: &str = "=== wisprcheap started";
const MAX_BYTES: u64 = 1_000_000;

type Sink = Box<dyn Fn(&str) + Send + Sync>;

static LOG_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
static SINKS: RwLock<Vec<Sink>> = RwLock::new(Vec::new());

pub fn log_file_path(base_dir: &Path) -> PathBuf {
    base_dir.join("wisprcheap.log")
}

/// Receive every logged line from now on.
pub fn add_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    SINKS.write().unwrap().push(Box::new(sink));
}

/// Start writing to `file` (rotated when bigger than 1 MB) and write the session marker.
pub fn setup(file: &Path) {
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(meta) = fs::metadata(file)
        && meta.len() > MAX_BYTES
    {
        let mut old = file.as_os_str().to_owned();
        old.push(".old");
        let _ = fs::rename(file, PathBuf::from(old));
    }
    *LOG_FILE.lock().unwrap() = Some(file.to_path_buf());
    emit(
        &format!(
            "\n{SESSION_MARKER} {} (pid {}) ===",
            chrono::Local::now().format("%-m/%-d/%Y, %-I:%M:%S %p"),
            std::process::id()
        ),
        false,
    );
}

/// Write one (possibly multi-line) message to the console, the log file and the sinks.
pub fn emit(text: &str, to_stderr: bool) {
    if to_stderr {
        let _ = writeln!(std::io::stderr().lock(), "{text}");
    } else {
        let _ = writeln!(std::io::stdout().lock(), "{text}");
    }
    if let Some(file) = LOG_FILE.lock().unwrap().as_ref()
        && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(file)
    {
        let _ = writeln!(f, "{text}");
    }
    if let Ok(sinks) = SINKS.read() {
        for sink in sinks.iter() {
            sink(text);
        }
    }
}

/// Local time like `2:33:40 PM`.
pub fn time() -> String {
    chrono::Local::now().format("%-I:%M:%S %p").to_string()
}

/// Lines logged by the most recent session (for startup error reports).
pub fn last_session_lines(file: &Path, max: usize) -> Vec<String> {
    let Ok(content) = fs::read_to_string(file) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines
        .iter()
        .rposition(|l| l.starts_with(SESSION_MARKER))
        .map(|i| i + 1)
        .unwrap_or(0);
    let kept: Vec<String> = lines[start..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();
    let skip = kept.len().saturating_sub(max);
    kept.into_iter().skip(skip).collect()
}

/// `[time] message` on stdout (like `log()` in the TypeScript version).
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::logging::emit(&format!("[{}] {}", $crate::logging::time(), format!($($arg)*)), false)
    };
}

/// `[time] message` on stderr.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::logging::emit(&format!("[{}] {}", $crate::logging::time(), format!($($arg)*)), true)
    };
}

/// Plain line on stdout (no timestamp).
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::logging::emit(&format!($($arg)*), false)
    };
}

/// Plain line on stderr (no timestamp).
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::logging::emit(&format!($($arg)*), true)
    };
}
