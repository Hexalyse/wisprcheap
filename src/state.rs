//! Small UI state remembered across restarts (not configuration), stored in the cache directory.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Selected translation pair id (e.g. "fr>en"), or None when translation is off.
    pub translation: Option<String>,
}

pub fn load_state() -> AppState {
    std::fs::read_to_string(crate::paths::state_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(state: &AppState) {
    let file = crate::paths::state_file();
    let result = (|| -> anyhow::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file, serde_json::to_string_pretty(state)?)?;
        Ok(())
    })();
    if let Err(e) = result {
        crate::warn!("[state] could not save: {e}");
    }
}
