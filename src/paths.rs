//! Where wisprcheap keeps its files.
//!
//! Portable layout: when a `config.yaml` sits next to the executable, that directory is the app
//! directory (config, `.env`, history, log) and the cache lives in `<exe dir>/.cache`.
//! Otherwise the standard per-user directories are used:
//! - Windows: `%APPDATA%\wisprcheap` and `%LOCALAPPDATA%\wisprcheap`
//! - Linux: `~/.config/wisprcheap` and `~/.cache/wisprcheap`

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct Paths {
    /// Default location of config.yaml / .env, and base directory when no config file is used.
    pub app_dir: PathBuf,
    /// Icons, UI state.
    pub cache_dir: PathBuf,
    pub portable: bool,
}

pub fn paths() -> &'static Paths {
    static PATHS: OnceLock<Paths> = OnceLock::new();
    PATHS.get_or_init(|| {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        if let Some(dir) = &exe_dir
            && dir.join("config.yaml").is_file()
        {
            return Paths {
                app_dir: dir.clone(),
                cache_dir: dir.join(".cache"),
                portable: true,
            };
        }
        let app_dir = dirs::config_dir()
            .map(|d| d.join("wisprcheap"))
            .or_else(|| exe_dir.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let cache_dir = dirs::cache_dir()
            .map(|d| d.join("wisprcheap"))
            .unwrap_or_else(|| app_dir.join(".cache"));
        Paths {
            app_dir,
            cache_dir,
            portable: false,
        }
    })
}

/// Instance suffix (WISPRCHEAP_INSTANCE) used by tests to run next to a real instance.
pub fn instance_suffix() -> Option<String> {
    std::env::var("WISPRCHEAP_INSTANCE")
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn icon_dir() -> PathBuf {
    paths().cache_dir.join("icons")
}

pub fn state_file() -> PathBuf {
    let name = match instance_suffix() {
        Some(s) => format!("state-{s}.json"),
        None => "state.json".to_string(),
    };
    paths().cache_dir.join(name)
}

/// `path.resolve(base, p)`: `p` if absolute, else `base/p`.
pub fn resolve(base: &Path, p: &str) -> PathBuf {
    let candidate = Path::new(p);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    std::path::absolute(&joined).unwrap_or(joined)
}

/// Current user name, sanitized for use in a pipe/socket name.
pub fn user_name() -> String {
    let raw = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".to_string());
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}
